//! Creating isolated connections to specific peers.

use std::{future::Future, net::SocketAddr};

use futures::future::TryFutureExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tower::{
    util::{BoxService, Oneshot},
    ServiceExt,
};

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    peer::{self, ConnectedAddr, HandshakeRequest},
    peer_set::ActiveConnectionCounter,
    BoxError, Config, Request, Response,
};

// TODO: tor feature
pub(crate) mod tor;

#[cfg(test)]
mod tests;

/// Creates a Zcash peer connection using the provided data stream.
/// This connection is completely isolated from all other node state.
///
/// The connection pool returned by [`init`](zebra_network::init)
/// should be used for all requests that
/// don't require isolated state or use of an existing TCP connection. However,
/// this low-level API is useful for custom network crawlers or Tor connections.
///
/// In addition to being completely isolated from all other node state, this
/// method also aims to be minimally distinguishable from other clients.
///
/// SECURITY TODO: check if the timestamp field can be zeroed, to remove another distinguisher (#3300)
///
/// Note that this method does not implement any timeout behavior, so callers may
/// want to layer it with a timeout as appropriate for their application.
///
/// # Inputs
///
/// - `network`: the Zcash [`Network`] used for this connection.
///
/// - `data_stream`: an existing data stream. This can be a non-anonymised TCP connection,
///                  or a Tor client [`DataStream`].
///
/// - `user_agent`: a valid BIP14 user-agent, e.g., the empty string.
pub fn connect_isolated<AsyncReadWrite>(
    network: Network,
    data_stream: AsyncReadWrite,
    user_agent: String,
) -> impl Future<Output = Result<BoxService<Request, Response, BoxError>, BoxError>>
where
    AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = Config {
        network,
        ..Config::default()
    };

    let handshake = peer::Handshake::builder()
        .with_config(config)
        .with_inbound_service(tower::service_fn(|_req| async move {
            Ok::<Response, BoxError>(Response::Nil)
        }))
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
    .map_ok(|client| BoxService::new(client.map_err(Into::into)))
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
/// Prefer [`connect_isolated_run_tor`](tor::connect_isolated_run_tor) if available.
pub fn connect_isolated_tcp_direct(
    network: Network,
    addr: SocketAddr,
    user_agent: String,
) -> impl Future<Output = Result<BoxService<Request, Response, BoxError>, BoxError>> {
    tokio::net::TcpStream::connect(addr)
        .err_into()
        .and_then(move |tcp_stream| connect_isolated(network, tcp_stream, user_agent))
}
