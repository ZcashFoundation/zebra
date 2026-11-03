//! Dialling version 2 peers for a state-snapshot bootstrap, before the peer
//! set exists (DESIGN.md §5, "Bootstrap dialer").
//!
//! The bootstrap downloads a state snapshot from a few configured peers
//! before the node has a state, an address book, or a peer set. It dials
//! them from client-only endpoints of its own, requires `NODE_SYNC_ARTIFACTS`
//! of each peer instead of `NODE_NETWORK`, and presents itself as a node with
//! no services, no chain, and no interest in transaction relay, that refuses
//! every request. The connections are ordinary version 2 connections,
//! presented as [`peer::Client`]s, and are closed with their endpoints
//! before the network starts.

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures::future::join_all;
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use tower::service_fn;
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::NoChainTip, diagnostic::task::CheckForPanics};

use crate::{
    connection_metrics::{
        record_connection_attempt_finished, record_connection_attempt_started, ConnectionDirection,
    },
    constants::HANDSHAKE_TIMEOUT,
    peer::{
        self,
        v2::{
            connection::{
                global_block_range_permits, global_hashes_permits, global_object_permits,
                spawn_connection, ConnectionParams, ObjectSizes,
            },
            handshake::{handshake_requiring, LocalIdentity, Role},
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
        },
        ConnectedAddr, MinimumPeerVersion,
    },
    peer_set::{ActiveConnectionCounter, ConnectionTracker},
    protocol::{
        external::{canonical_peer_addr, types::PeerServices, MAX_USER_AGENT_LENGTH},
        v2::types::ErrorCode,
    },
    quic::{self, Windows},
    BoxError, Config, PeerSocketAddr, Request, Response,
};

/// The version 2 peers a state-snapshot bootstrap downloads from, dialled
/// before the peer set exists.
#[derive(Debug)]
pub struct BootstrapPeers {
    /// The connected peers, each as a client the bootstrap sends its requests
    /// to.
    pub clients: Vec<(PeerSocketAddr, peer::Client)>,

    /// The client-only endpoints the connections were dialled from, one per
    /// address family among the peers, closed by [`Self::close`].
    endpoints: Vec<quinn::Endpoint>,
}

impl BootstrapPeers {
    /// Closes every connection, then the endpoints with `NO_ERROR`, and waits
    /// up to `BOOTSTRAP_CLOSE_GRACE` (two seconds) for the peers to
    /// acknowledge, so that the network starts without a lingering endpoint.
    pub async fn close(self) {
        drop(self.clients);
        for endpoint in &self.endpoints {
            endpoint.close(
                quic::app_error_code(ErrorCode::NoError),
                b"bootstrap finished",
            );
        }
        let idle = join_all(self.endpoints.iter().map(quinn::Endpoint::wait_idle));
        if tokio::time::timeout(BOOTSTRAP_CLOSE_GRACE, idle)
            .await
            .is_err()
        {
            tracing::debug!(
                grace = ?BOOTSTRAP_CLOSE_GRACE,
                "bootstrap peers did not acknowledge the close in time"
            );
        }
    }
}

/// How long [`BootstrapPeers::close`] waits for the peers to acknowledge the
/// close.
pub const BOOTSTRAP_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// The most peers a bootstrap dials (DESIGN.md §5, "Bootstrap dialer"): the
/// scheduler downloads from at most this many at once.
pub const MAX_BOOTSTRAP_PEERS: usize = 8;

/// Dials `peers` over the version 2 protocol for a state-snapshot bootstrap,
/// and returns the connected clients.
///
/// The bootstrap runs before the peer set, from client-only endpoints with
/// bulk receive windows, one per address family among the peers, and
/// requires `NODE_SYNC_ARTIFACTS` of every peer instead of `NODE_NETWORK`.
/// It presents itself to the peers as a node with no services, no chain, and
/// no interest in transaction relay, that refuses every request; the peers'
/// misbehavior is scored, and a peer that reaches the threshold is
/// disconnected, but there is no address book to ban it in. Peers are dialled
/// once per IP address, in order, at most `MAX_BOOTSTRAP_PEERS` (eight) of
/// them, all at once; each attempt is bounded by the transport and application
/// handshake timeouts.
///
/// Returns the peers that completed the handshake, which may be none: each
/// failed attempt is logged. Fails if there is no peer to dial, the user
/// agent is over the protocol's limit, or an endpoint cannot be bound. The
/// caller closes the clients and the endpoints with [`BootstrapPeers::close`]
/// before starting the network.
pub async fn v2_bootstrap_clients(
    config: &Config,
    peers: &[PeerSocketAddr],
    user_agent: String,
) -> Result<BootstrapPeers, BoxError> {
    // The record encoder relies on this bound, which the peer set checks when it starts.
    if user_agent.len() > MAX_USER_AGENT_LENGTH {
        return Err(format!(
            "the version 2 user agent {user_agent:?} is {} bytes, over the protocol's limit of \
             {MAX_USER_AGENT_LENGTH} bytes",
            user_agent.len(),
        )
        .into());
    }
    let peers = select_peers(peers);
    if peers.is_empty() {
        return Err("no state snapshot bootstrap peers to dial".into());
    }

    let network = &config.network;
    let identity = LocalIdentity {
        user_agent,
        services: PeerServices::empty(),
        relay: false,
    };
    let endpoints = Endpoints::for_peers(config, &peers)?;
    // A peer whose address family has no endpoint, because that family could not be bound on
    // this host, is skipped with a warning from `for_peers`.
    let peers: Vec<PeerSocketAddr> = peers
        .into_iter()
        .filter(|addr| endpoints.for_addr(*addr).is_some())
        .collect();
    if peers.is_empty() {
        return Err(
            "no state snapshot bootstrap peer is reachable from this host's address families"
                .into(),
        );
    }

    // `NoChainTip` reports no tip: the node has no state yet, so it counts as syncing, which is
    // what makes the handshake require services of the peers, and its `init` records carry a
    // start height of 0.
    let minimum_peer_version = MinimumPeerVersion::new(NoChainTip, network);
    let nonces = NonceRegistry::spawn(peers.len());
    // The report receiver is dropped: there is no address book to ban peers in yet. A peer whose
    // score reaches the threshold is still disconnected with `MISBEHAVIOR` by its connection.
    let (misbehavior_tx, _dropped) = mpsc::channel(1);
    let params = ConnectionParams {
        inbound_service: service_fn(refuse_request),
        // Nothing listens for inventory: the bootstrap has no peer set to route by it.
        inv_collector: broadcast::channel(1).0,
        genesis_hash: network.genesis_hash(),
        misbehavior: MisbehaviorScores::spawn(misbehavior_tx, peers.len()),
        // The bootstrap keeps no addresses and relays none.
        address_book: None,
        discovery: false,
        bans: watch::channel(Default::default()).1,
        addr_relay: None,
        // It serves nothing: `local_services` is empty and the inbound service refuses.
        serve_block_ranges: false,
        block_range_permits: global_block_range_permits(),
        hashes_permits: global_hashes_permits(),
        object_source: None,
        local_services: identity.services,
        object_permits: global_object_permits(),
    };

    let mut counter =
        ActiveConnectionCounter::new_counter_with(peers.len(), "Bootstrap v2 Connections");
    let mut attempts = JoinSet::new();
    for addr in peers {
        let endpoint = endpoints
            .for_addr(addr)
            .expect("peers without an endpoint for their address family were filtered out above")
            .clone();
        attempts.spawn(
            connect_once(
                endpoint,
                addr,
                network.clone(),
                identity.clone(),
                minimum_peer_version.clone(),
                nonces.clone(),
                params.clone(),
                counter.track_connection(),
            )
            .in_current_span(),
        );
    }

    let mut clients = Vec::new();
    while let Some(finished) = attempts.join_next().await {
        // A panic in an attempt is this node's bug, and is resumed here.
        if let Ok((addr, result)) = finished.panic_if_task_has_panicked() {
            match result {
                Ok(client) => {
                    tracing::info!(?addr, "connected to state snapshot bootstrap peer");
                    clients.push((addr, client));
                }
                Err(error) => {
                    tracing::info!(?addr, %error, "state snapshot bootstrap peer connection failed");
                }
            }
        }
    }

    Ok(BootstrapPeers {
        clients,
        endpoints: endpoints.into_vec(),
    })
}

/// The peers a bootstrap dials, from the configured `peers`: canonical
/// addresses, the first for each IP address in order, at most
/// [`MAX_BOOTSTRAP_PEERS`] of them (DESIGN.md §5, "Bootstrap dialer").
pub(in crate::peer_set::initialize) fn select_peers(
    peers: &[PeerSocketAddr],
) -> Vec<PeerSocketAddr> {
    let mut ips: HashSet<IpAddr> = HashSet::new();
    peers
        .iter()
        .map(|addr| canonical_peer_addr(*addr))
        .filter(|addr| ips.insert(addr.ip()))
        .take(MAX_BOOTSTRAP_PEERS)
        .collect()
}

/// The bootstrap's client-only endpoints, one per address family it dials.
struct Endpoints {
    v4: Option<quinn::Endpoint>,
    v6: Option<quinn::Endpoint>,
}

impl Endpoints {
    /// Binds a client-only endpoint on an unspecified address of each
    /// address family among `peers`, with the bootstrap's windows.
    ///
    /// A family this host cannot bind, such as IPv6 on a host without it, is
    /// skipped with a warning, so the peers of the other family are still
    /// dialled. Fails only if the network has no version 2 transport, which
    /// no family can work around.
    fn for_peers(config: &Config, peers: &[PeerSocketAddr]) -> Result<Self, BoxError> {
        let bind = |addr: IpAddr| -> Result<Option<quinn::Endpoint>, BoxError> {
            match quic::client_endpoint(
                &config.network,
                SocketAddr::new(addr, 0),
                Windows::BOOTSTRAP,
            ) {
                Ok(endpoint) => {
                    tracing::debug!(
                        local_addr = ?endpoint.local_addr().ok(),
                        "opened a client-only version 2 (QUIC) endpoint for the state snapshot \
                         bootstrap"
                    );
                    Ok(Some(endpoint))
                }
                Err(error @ quic::EndpointError::NoAlpnIdentifier { .. }) => Err(error.into()),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        family = if addr.is_ipv4() { "IPv4" } else { "IPv6" },
                        "could not open a client-only version 2 (QUIC) endpoint for the state \
                         snapshot bootstrap, skipping the bootstrap peers of that address family"
                    );
                    Ok(None)
                }
            }
        };
        let v4 = if peers.iter().any(|addr| addr.is_ipv4()) {
            bind(Ipv4Addr::UNSPECIFIED.into())?
        } else {
            None
        };
        let v6 = if peers.iter().any(|addr| addr.is_ipv6()) {
            bind(Ipv6Addr::UNSPECIFIED.into())?
        } else {
            None
        };
        Ok(Endpoints { v4, v6 })
    }

    /// The endpoint that dials `addr`, if its address family has one.
    fn for_addr(&self, addr: PeerSocketAddr) -> Option<&quinn::Endpoint> {
        if addr.is_ipv4() {
            self.v4.as_ref()
        } else {
            self.v6.as_ref()
        }
    }

    /// The bound endpoints.
    fn into_vec(self) -> Vec<quinn::Endpoint> {
        self.v4.into_iter().chain(self.v6).collect()
    }
}

/// The bootstrap's inbound service: it refuses every request, so its
/// connections reset the peer's request streams with `REFUSED`.
fn refuse_request(request: Request) -> futures::future::Ready<Result<Response, BoxError>> {
    futures::future::ready(Err(format!(
        "the state snapshot bootstrap serves no requests, refusing {}",
        request.command()
    )
    .into()))
}

/// Connects to `addr` from `endpoint`, runs the handshake requiring
/// `NODE_SYNC_ARTIFACTS`, and spawns the connection task, returning the
/// peer's address and its client, or the attempt's error.
#[allow(clippy::too_many_arguments)]
async fn connect_once<S>(
    endpoint: quinn::Endpoint,
    addr: PeerSocketAddr,
    network: zebra_chain::parameters::Network,
    identity: LocalIdentity,
    mut minimum_peer_version: MinimumPeerVersion<NoChainTip>,
    nonces: NonceRegistry,
    params: ConnectionParams<S>,
    mut tracker: ConnectionTracker,
) -> (PeerSocketAddr, Result<peer::Client, BoxError>)
where
    S: tower::Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    record_connection_attempt_started(&network, ConnectionDirection::Outbound, addr);
    let result = async {
        // The QUIC handshake has its own timeout inside `connect`; the application handshake's
        // deadline runs from the connection's establishment, as the draft asks.
        let connection = quic::connect(&endpoint, *addr).await?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let completed = handshake_requiring(
            &connection,
            ConnectedAddr::new_outbound_direct(addr),
            Role::Initiator,
            &identity,
            &mut minimum_peer_version,
            &nonces,
            deadline,
            PeerServices::NODE_SYNC_ARTIFACTS,
        )
        .await?;
        tracker.mark_open();
        let (client, _closer) = spawn_connection(
            connection,
            completed,
            tracker,
            params,
            ObjectSizes::spawn(),
            None,
        );
        Ok::<_, BoxError>(client)
    }
    .await;
    record_connection_attempt_finished(
        &network,
        ConnectionDirection::Outbound,
        addr,
        result.as_ref().err(),
    );
    (addr, result)
}
