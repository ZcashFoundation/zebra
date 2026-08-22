//! The version 2 protocol (QUIC) peer source.
//!
//! When enabled by the [`Config`], a QUIC endpoint runs alongside the legacy
//! TCP listener: it accepts inbound v2 connections on the UDP port of the
//! listen address, and dials the configured initial v2 peers. Both produce
//! ordinary peer [`Client`](crate::peer::Client)s, which join the peer set
//! alongside legacy peers.

use std::{net::IpAddr, pin::Pin, sync::Arc};

use futures::{future::FutureExt, stream::FuturesUnordered, Future, SinkExt, StreamExt};
use indexmap::IndexMap;
use tokio::sync::watch;
use tower::{Service, ServiceExt};
use tracing::{debug, info, warn, Instrument};

use zebra_chain::{chain_tip::ChainTip, diagnostic::task::WaitForPanics};

use crate::{
    connection_metrics::{record_connection_attempt_finished, ConnectionDirection},
    constants,
    peer::{
        v2::{V2HandshakeRequest, V2Handshaker},
        ConnectedAddr,
    },
    peer_set::{
        initialize::{inbound_admission, recent_by_ip, DiscoveredPeer},
        limit::SharedConnectionCounter,
    },
    protocol::v2::quic,
    BoxError, Config, Request, Response,
};

/// Opens the version 2 QUIC endpoint.
///
/// When listening is enabled, the endpoint uses the UDP port of the TCP
/// listen address, as anticipated by the draft ZIP's deployment plan.
/// Otherwise, an unspecified port is used for outbound connections only.
pub(super) fn new_v2_endpoint(
    config: &Config,
    listen_addr: std::net::SocketAddr,
) -> Result<quinn::Endpoint, BoxError> {
    let bind_addr = if config.v2_listen {
        listen_addr
    } else {
        use std::net::{Ipv4Addr, Ipv6Addr};
        let unspecified: IpAddr = if listen_addr.is_ipv4() {
            Ipv4Addr::UNSPECIFIED.into()
        } else {
            Ipv6Addr::UNSPECIFIED.into()
        };
        std::net::SocketAddr::new(unspecified, 0)
    };

    let endpoint = quic::new_endpoint(bind_addr, &config.network).map_err(|error| {
        warn!(%error, ?bind_addr, "failed to open the v2 protocol QUIC endpoint");
        error
    })?;

    if config.v2_listen {
        info!(addr = ?bind_addr, "opened QUIC endpoint for v2 protocol connections");
    }

    Ok(endpoint)
}

/// Runs the version 2 QUIC endpoint: dials the configured initial v2 peers,
/// and accepts inbound v2 connections if listening is enabled.
///
/// The inbound and outbound connection counters are shared with the legacy
/// transport, so both transports together stay within the configured limits.
///
/// Returns an error if the endpoint cannot be created.
pub(super) async fn run_v2_endpoint<S, C>(
    config: Config,
    endpoint: quinn::Endpoint,
    handshaker: V2Handshaker<S, C>,
    peerset_tx: futures::channel::mpsc::Sender<DiscoveredPeer>,
    bans_receiver: watch::Receiver<Arc<IndexMap<crate::peer_book::BanKey, std::time::Instant>>>,
    active_inbound_connections: SharedConnectionCounter,
    recent_inbound_connections: watch::Sender<recent_by_ip::RecentByIp>,
) -> Result<(), BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
{
    // Accept inbound v2 connections.
    //
    // The inbound handshakes currently in progress; above the threshold,
    // unvalidated addresses are asked to retry with a token.
    let pending_handshakes = crate::peer_set::SlotCounter::default();

    // The per-connection handshake tasks are supervised, so a panic in any
    // of them stops the node instead of silently reducing the transport's
    // connectivity.
    let mut handshake_tasks: FuturesUnordered<Pin<Box<dyn Future<Output = ()> + Send>>> =
        FuturesUnordered::new();
    // Keeping an unresolved future in the pool means the stream never terminates.
    handshake_tasks.push(futures::future::pending().boxed());

    loop {
        // Check for panics in finished tasks, before accepting new connections.
        let incoming = tokio::select! {
            biased;
            joined = handshake_tasks.next() => match joined {
                Some(()) => continue,
                None => unreachable!(
                    "handshake_tasks never terminates, because it contains a future that never resolves"
                ),
            },

            // This future must wait until new connections are available: it can't have a timeout.
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                // The endpoint was closed: this node is shutting down.
                None => return Ok(()),
            },
        };
        // # Security
        //
        // Above a threshold of concurrently pending handshakes — as under a
        // handshake flood — require source address validation with a retry
        // token before accepting: a spoofed-source attempt then costs this
        // node only a stateless retry packet, not a TLS handshake.
        if !incoming.remote_address_validated()
            && pending_handshakes.count()
                >= crate::protocol::v2::constants::MAX_PENDING_INBOUND_HANDSHAKES
        {
            // An error means the connection attempt was already unusable.
            let _ = incoming.retry();
            continue;
        }

        let Some((addr, _canonical_ip)) = inbound_admission::screen_inbound_addr(
            &config.network,
            incoming.remote_address(),
            &bans_receiver,
        ) else {
            incoming.refuse();
            continue;
        };

        // # Security
        //
        // The inbound counter is shared with the legacy transport, so both
        // transports together stay within the configured limit, and the
        // per-IP limit stops one peer from occupying the v2 connections.
        let Some(connection_tracker) = inbound_admission::try_reserve_public_inbound_slot(
            &config.network,
            addr,
            config.peerset_inbound_connection_limit(),
            &active_inbound_connections,
            &recent_inbound_connections,
        ) else {
            incoming.refuse();
            // Allow invalid connections to be cleared quickly, but still put a
            // limit on our CPU and network usage from failed connections.
            tokio::time::sleep(constants::MIN_INBOUND_PEER_FAILED_CONNECTION_INTERVAL).await;
            continue;
        };
        let network = config.network.clone();
        let handshaker = handshaker.clone();
        let mut peerset_tx = peerset_tx.clone();

        // Counts this attempt as a pending handshake until it completes.
        let pending_slot = pending_handshakes.claim();

        let handshake_task = tokio::spawn(
            async move {
                let connection = match quic::accept(incoming, &network).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        record_connection_attempt_finished(
                            &network,
                            ConnectionDirection::Inbound,
                            addr,
                            Some(&error),
                        );
                        debug!(%error, ?addr, "inbound v2 connection failed");
                        return;
                    }
                };

                let connected_addr = ConnectedAddr::new_inbound_direct(addr);

                let client = handshaker
                    .oneshot(V2HandshakeRequest {
                        connection,
                        connected_addr,
                        connection_tracker,
                    })
                    .await;
                drop(pending_slot);

                record_connection_attempt_finished(
                    &network,
                    ConnectionDirection::Inbound,
                    addr,
                    client.as_ref().err(),
                );

                match client {
                    Ok(client) => {
                        let _ = peerset_tx.send((addr, client)).await;
                    }
                    Err(error) => debug!(%error, ?addr, "inbound v2 handshake failed"),
                }
            }
            .in_current_span(),
        );
        handshake_tasks.push(handshake_task.wait_for_panics().boxed());

        // Rate-limit inbound connections, like the legacy listener.
        tokio::time::sleep(constants::MIN_INBOUND_PEER_CONNECTION_INTERVAL).await;

        // Security: Let other tasks run after each connection is processed,
        // so remote peers cannot starve other Zebra tasks using inbound
        // connections. (Sleeps are not guaranteed to schedule other tasks.)
        tokio::task::yield_now().await;
    }
}
