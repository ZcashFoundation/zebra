//! Uses tor to create isolated and anonymised connections to specific peers.

use std::sync::{Arc, Mutex};

use arti_client::{DataStream, TorAddr, TorClient, TorClientConfig};
use tor_rtcompat::tokio::TokioRuntimeHandle;
use tower::{util::BoxService, Service};

use zebra_chain::parameters::Network;

use crate::{connect_isolated, connect_isolated_with_inbound, BoxError, Request, Response};

#[cfg(test)]
mod tests;

lazy_static::lazy_static! {
    /// The shared isolated [`TorClient`] instance.
    ///
    /// TODO: turn this into a tower service that takes a hostname, and returns an [`arti_client::DataStream`]
    ///       (or a task that updates a watch channel when it's done?)
    pub static ref SHARED_TOR_CLIENT: Arc<Mutex<Option<TorClient<TokioRuntimeHandle>>>> =
        Arc::new(Mutex::new(None));
}

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
    let tor_stream = new_tor_stream(hostname).await?;

    // Calling connect_isolated_tor_with_inbound causes lifetime issues.
    //
    // TODO: fix the lifetime issues, and call connect_isolated_tor_with_inbound
    //       so the behaviour of both functions is consistent.
    connect_isolated(network, tor_stream, user_agent).await
}

/// Creates an isolated Zcash peer connection to `hostname` via Tor.
/// This function is for testing purposes only.
///
/// See [`connect_isolated_with_inbound`] and [`connect_isolated_tor`] for details.
///
/// # Privacy
///
/// This function can make the isolated connection send different responses to peers,
/// which makes it stand out from other isolated connections from other peers.
pub async fn connect_isolated_tor_with_inbound<InboundService>(
    network: Network,
    hostname: String,
    user_agent: String,
    inbound_service: InboundService,
) -> Result<BoxService<Request, Response, BoxError>, BoxError>
where
    InboundService:
        Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    InboundService::Future: Send,
{
    let tor_stream = new_tor_stream(hostname).await?;

    connect_isolated_with_inbound(network, tor_stream, user_agent, inbound_service).await
}

/// Creates a Zcash peer connection to `hostname` via Tor, and returns a tor stream.
///
/// See [`connect_isolated`] for details.
async fn new_tor_stream(hostname: String) -> Result<DataStream, BoxError> {
    let addr = TorAddr::from(hostname)?;

    // Initialize or clone the shared tor client instance
    let tor_client = match cloned_tor_client() {
        Some(tor_client) => tor_client,
        None => new_tor_client().await?,
    };

    let tor_stream = tor_client.connect(addr, None).await?;

    Ok(tor_stream)
}

/// Returns a new tor client instance, and updates [`struct@SHARED_TOR_CLIENT`].
///
/// If there is a bootstrap error, [`struct@SHARED_TOR_CLIENT`] is not modified.
async fn new_tor_client() -> Result<TorClient<TokioRuntimeHandle>, BoxError> {
    let runtime = tokio::runtime::Handle::current();
    let runtime = TokioRuntimeHandle::new(runtime);
    let tor_client = TorClient::bootstrap(runtime, TorClientConfig::default()).await?;

    // # Correctness
    //
    // It is ok for multiple tasks to race, because all tor clients have identical configs.
    // And all connections are isolated, regardless of whether they use a new or cloned client.
    // (Any replaced clients will be dropped.)
    let mut shared_tor_client = SHARED_TOR_CLIENT
        .lock()
        .expect("panic in shared tor client mutex guard");
    *shared_tor_client = Some(tor_client.isolated_client());

    Ok(tor_client)
}

/// Returns an isolated tor client instance by cloning [`struct@SHARED_TOR_CLIENT`].
///
/// If [`new_tor_client`] has not run successfully yet, returns `None`.
fn cloned_tor_client() -> Option<TorClient<TokioRuntimeHandle>> {
    SHARED_TOR_CLIENT
        .lock()
        .expect("panic in shared tor client mutex guard")
        .as_ref()
        .map(TorClient::isolated_client)
}
