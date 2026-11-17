//! Independently maintained, slot-reserved operator peers.

use std::{future::Future, sync::Mutex};

use futures::{stream::FuturesUnordered, TryStreamExt};
use tower::{limit::rate::Rate, limit::RateLimit};

use super::*;
use crate::protocol::external::connection_limit_key;

/// Share one attempt budget across every outbound source, without serializing handshakes.
pub(super) fn paced_connector<C>(
    connector: C,
) -> impl Service<
    OutboundConnectorRequest,
    Response = C::Response,
    Error = BoxError,
    Future: Send + 'static,
> + Clone
where
    C: Service<OutboundConnectorRequest, Error = BoxError> + Send + 'static,
    C::Future: Send + 'static,
{
    let connector = Arc::new(tokio::sync::Mutex::new(RateLimit::new(
        connector,
        Rate::new(1, constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL),
    )));
    tower::service_fn(move |request| {
        let connector = connector.clone();
        async move {
            let handshake = {
                let mut connector = connector.lock().await;
                connector.ready().await?.call(request)
            };
            handshake.await
        }
    })
}

/// Keeps one connection per configured target, cancelling all work when dropped.
///
/// The resolver is injected so DNS changes and failures can be tested without network access.
#[allow(clippy::too_many_arguments)]
pub(super) async fn maintain<C, R, F>(
    config: Config,
    outbound_connector: C,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    address_book_updater: AddressBookChangeSender,
    bans: watch::Receiver<BanList>,
    connected_peers: watch::Receiver<super::super::AdvertPeers>,
    resolve: R,
) -> Result<(), BoxError>
where
    C: Service<OutboundConnectorRequest, Response = DiscoveredPeer, Error = BoxError>
        + Send
        + 'static,
    C::Future: Send + 'static,
    R: Fn(String) -> F + Send + Sync,
    F: Future<Output = HashSet<PeerSocketAddr>> + Send,
{
    let connector = tokio::sync::Mutex::new(outbound_connector);
    // Include pending handshakes so DNS aliases cannot race each other to the same endpoint/IP.
    let connecting = Mutex::new(HashSet::<PeerSocketAddr>::new());
    let targets = config.persistent_peers.iter().map(|host| {
        let connector = &connector;
        let mut peerset_tx = peerset_tx.clone();
        let connecting = &connecting;
        let config = &config;
        let resolve = &resolve;
        let bans = &bans;
        let connected_peers = &connected_peers;
        let address_book_updater = &address_book_updater;
        async move {
            let mut connection =
                ActiveConnectionCounter::new_counter_with(1, "Persistent Connection");
            loop {
                let mut addresses = resolve(host.clone()).await.into_iter();
                while addresses.len() != 0 {
                    // Drain failed trackers before reserving the next connection.
                    connection.update_count();
                    let attempt = {
                        let mut connector = connector.lock().await;
                        connector.ready().await?;
                        let peers = connected_peers.borrow();
                        let mut pending = connecting
                            .lock()
                            .expect("no panic while reserving an endpoint");
                        addresses
                            .by_ref()
                            .find(|addr| {
                                if (config.network.is_regtest() && !addr.is_localhost())
                                    || PeerPreference::new(*addr, config.network.clone()).is_err()
                                    || bans.borrow().is_banned(addr.ip())
                                    || peers.contains_key(addr)
                                    || pending.contains(addr)
                                {
                                    return false;
                                }
                                let group = connection_limit_key(addr.ip());
                                let same_ip = peers
                                    .keys()
                                    .chain(pending.iter().filter(|addr| !peers.contains_key(*addr)))
                                    .filter(|peer| connection_limit_key(peer.ip()) == group)
                                    .count();
                                if same_ip >= config.max_connections_per_ip {
                                    return false;
                                }
                                pending.insert(*addr);
                                true
                            })
                            .map(|addr| {
                                let future = connector.call(OutboundConnectorRequest {
                                    addr,
                                    connection_tracker: connection.track_connection(),
                                });
                                (addr, future)
                            })
                    };
                    if let Some((addr, handshake)) = attempt {
                        // Record attempts for crawler pacing; peer-set admission handles source races.
                        address_book_updater
                            .send(MetaAddr::new_reconnect(addr))
                            .await?;
                        record_connection_attempt_started(
                            &config.network,
                            ConnectionDirection::Outbound,
                            addr,
                        );
                        let result = handshake.await;
                        record_connection_attempt_finished(
                            &config.network,
                            ConnectionDirection::Outbound,
                            addr,
                            result.as_ref().err(),
                        );
                        let disconnected = match result {
                            Ok(peer) => {
                                if let Err(error) = peerset_tx.send(peer).await {
                                    return Err::<(), BoxError>(error.into());
                                }
                                // The tracker belongs to the actual connection, not this maintenance task.
                                while connection.update_count() != 0 {
                                    sleep(constants::DNS_LOOKUP_TIMEOUT).await;
                                }
                                true
                            }
                            Err(error) => {
                                debug!(?host, ?addr, ?error, "persistent peer connection failed");
                                address_book_updater
                                    .send(MetaAddr::new_errored(addr, None))
                                    .await?;
                                false
                            }
                        };
                        connecting
                            .lock()
                            .expect("no panic while releasing an endpoint")
                            .remove(&addr);
                        if disconnected {
                            break;
                        }
                    }
                }
                // Every retry, including empty/failed DNS, re-resolves the configured hostname.
                sleep(constants::MIN_PEER_RECONNECTION_DELAY).await;
            }
        }
    });
    targets.collect::<FuturesUnordered<_>>().try_collect().await
}
