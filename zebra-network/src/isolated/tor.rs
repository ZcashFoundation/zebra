//! Uses tor to create isolated and anonymised connections to specific peers.

use arti_client::{TorAddr, TorClient, TorClientConfig};
use tower::util::BoxService;

use zebra_chain::parameters::Network;

use crate::{connect_isolated, BoxError, Request, Response};

#[cfg(test)]
mod tests;

/// Creates a Zcash peer connection to `hostname` via Tor.
/// This connection is completely isolated from all other node state.
///
/// See [`connect_isolated`] for details.
///
/// # Privacy
///
/// The sender IP address is anonymised using Tor.
/// But transactions sent over this connection can still be linked to the receiving IP address
/// by passive internet observers.
/// This happens because the Zcash network protocol uses unencrypted TCP connections.
///
/// `hostname` should be a DNS name for the Tor exit to look up, or a hard-coded IP address.
/// If the application does a local DNS lookup on a hostname, and passes the IP address to this function,
/// passive internet observers can link the hostname to the sender's IP address.
///
/// For details, see
/// [`TorAddr`](https://tpo.pages.torproject.net/core/doc/rust/arti_client/struct.TorAddr.html).
pub async fn connect_isolated_tor(
    network: Network,
    hostname: String,
    user_agent: String,
) -> Result<BoxService<Request, Response, BoxError>, BoxError> {
    let addr = TorAddr::from(hostname)?;
    let runtime = tor_rtcompat::tokio::current_runtime()?;

    let tor_client = TorClient::bootstrap(runtime, TorClientConfig::default()).await?;
    let tor_stream = tor_client.connect(addr, None).await?;

    connect_isolated(network, tor_stream, user_agent).await
}
