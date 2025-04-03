//! Creating isolated connections to specific peers.

use std::future::Future;

use futures::future::TryFutureExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tower::{util::Oneshot, Service};

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    peer::{self, Client, ConnectedAddr, HandshakeRequest},
    peer_set::ActiveConnectionCounter,
    BoxError, Config, PeerSocketAddr, Request, Response,
};

// Wait until `arti-client`'s dependency `x25519-dalek v1.2.0` is updated to a higher version. (#5492)
// #[cfg(feature = "tor")]
// pub(crate) mod tor;

#[cfg(test)]
mod tests;

/// Creates a Zcash peer connection using the provided data stream.
/// This connection is completely isolated from all other node state.
///
/// The connection pool returned by [`init`](crate::init)
/// should be used for all requests that
/// don't require isolated state or use of an existing TCP connection. However,
/// this low-level API is useful for custom network crawlers or Tor connections.
///
/// In addition to being completely isolated from all other node state, this
/// function also aims to be minimally distinguishable from other clients.
///
/// SECURITY TODO: check if the timestamp field can be zeroed, to remove another distinguisher (#3300)
///
/// Note that this function does not implement any timeout behavior, so callers may
/// want to layer it with a timeout as appropriate for their application.
///
/// # Inputs
///
/// - `network`: the Zcash [`Network`] used for this connection.
///
/// - `data_stream`: an existing data stream. This can be a non-anonymised TCP connection,
///   or a Tor client `arti_client::DataStream`.
///
/// - `user_agent`: a valid BIP14 user-agent, e.g., the empty string.
pub fn connect_isolated<PeerTransport>(
    network: &Network,
    data_stream: PeerTransport,
    user_agent: String,
) -> impl Future<Output = Result<Client, BoxError>>
where
    PeerTransport: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let nil_inbound_service =
        tower::service_fn(|_req| async move { Ok::<Response, BoxError>(Response::Nil) });

    connect_isolated_with_inbound(network, data_stream, user_agent, nil_inbound_service)
}

/// Creates an isolated Zcash peer connection using the provided data stream.
/// This function is for testing purposes only.
///
/// See [`connect_isolated`] for details.
///
/// # Additional Inputs
///
/// - `inbound_service`: a [`tower::Service`] that answers inbound requests from the connected peer.
///
/// # Privacy
///
/// This function can make the isolated connection send different responses to peers,
/// which makes it stand out from other isolated connections from other peers.
pub fn connect_isolated_with_inbound<PeerTransport, InboundService>(
    network: &Network,
    data_stream: PeerTransport,
    user_agent: String,
    inbound_service: InboundService,
) -> impl Future<Output = Result<Client, BoxError>>
where
    PeerTransport: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    InboundService:
        Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    InboundService::Future: Send,
{
    let config = Config {
        network: network.clone(),
        ..Config::default()
    };

    let handshake = peer::Handshake::builder()
        .with_config(config)
        .with_inbound_service(inbound_service)
        .with_user_agent(user_agent)
        .with_latest_chain_tip(NoChainTip)
        .finish()
        .expect("provided mandatory builder parameters");

    // Don't send or track any metadata about the connection
    let connected_addr = ConnectedAddr::new_isolated();
    let connection_tracker = ActiveConnectionCounter::new_counter().track_connection();

    Oneshot::new(
        handshake,
        HandshakeRequest {
            data_stream,
            connected_addr,
            connection_tracker,
        },
    )
}

/// Creates a direct TCP Zcash peer connection to `addr`.
/// This connection is completely isolated from all other node state.
///
/// See [`connect_isolated`] for details.
///
/// # Privacy
///
/// Transactions sent over this connection can be linked to the sending and receiving IP address
/// by passive internet observers.
///
///
/// Prefer `connect_isolated_tor` if available.
pub fn connect_isolated_tcp_direct(
    network: &Network,
    addr: impl Into<PeerSocketAddr>,
    user_agent: String,
) -> impl Future<Output = Result<Client, BoxError>> {
    let nil_inbound_service =
        tower::service_fn(|_req| async move { Ok::<Response, BoxError>(Response::Nil) });

    connect_isolated_tcp_direct_with_inbound(network, addr, user_agent, nil_inbound_service)
}

/// Creates an isolated Zcash peer connection using the provided data stream.
/// This function is for testing purposes only.
///
/// See [`connect_isolated_with_inbound`] and [`connect_isolated_tcp_direct`] for details.
///
/// # Privacy
///
/// This function can make the isolated connection send different responses to peers,
/// which makes it stand out from other isolated connections from other peers.
pub fn connect_isolated_tcp_direct_with_inbound<InboundService>(
    network: &Network,
    addr: impl Into<PeerSocketAddr>,
    user_agent: String,
    inbound_service: InboundService,
) -> impl Future<Output = Result<Client, BoxError>>
where
    InboundService:
        Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    InboundService::Future: Send,
{
    let addr = addr.into();
    let network = network.clone();

    tokio::net::TcpStream::connect(*addr)
        .err_into()
        .and_then(move |tcp_stream| {
            connect_isolated_with_inbound(&network, tcp_stream, user_agent, inbound_service)
        })
}
