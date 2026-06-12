//! A peer connection service wrapper type to handle load tracking and provide access to the
//! reported protocol version.

use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use tower::{
    load::{Load, PeakEwma},
    Service,
};

use zebra_chain::block::Height;

use crate::{
    constants::{EWMA_DECAY_TIME_NANOS, EWMA_DEFAULT_RTT},
    peer::{Client, ConnectionInfo},
    protocol::external::types::Version,
};

#[cfg(test)]
mod tests;

/// A client service wrapper that keeps track of its load.
///
/// It also keeps track of the peer's reported protocol version.
#[derive(Debug)]
pub struct LoadTrackedClient {
    /// A service representing a connected peer, wrapped in a load tracker.
    service: PeakEwma<Client>,

    /// The metadata for the connected peer `service`.
    connection_info: Arc<ConnectionInfo>,

    /// The highest block height this peer has demonstrably served us over the
    /// connection's lifetime, or zero if it hasn't served us any blocks yet.
    ///
    /// Shared with the block-download response futures routed to this peer,
    /// which raise it when a `Blocks` response arrives. Only ever raised, never
    /// lowered, so it is a trusted direct signal rather than gossip.
    live_height: Arc<AtomicU32>,

    /// The number of blocks downloaded from this peer over the connection's lifetime.
    ///
    /// Shared with the block-download response futures routed to this peer,
    /// which increment it when a `Blocks` response arrives. Used for sync
    /// diagnostics.
    blocks_received: Arc<AtomicU64>,
}

/// Create a new [`LoadTrackedClient`] wrapping the provided `client` service.
impl From<Client> for LoadTrackedClient {
    fn from(client: Client) -> Self {
        let connection_info = client.connection_info.clone();

        let service = PeakEwma::new(
            client,
            EWMA_DEFAULT_RTT,
            EWMA_DECAY_TIME_NANOS,
            tower::load::CompleteOnResponse::default(),
        );

        LoadTrackedClient {
            service,
            connection_info,
            live_height: Arc::new(AtomicU32::new(0)),
            blocks_received: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl LoadTrackedClient {
    /// Retrieve the peer's reported protocol version.
    pub fn remote_version(&self) -> Version {
        self.connection_info.remote.version
    }

    /// Returns the peer's current chain height, as far as we can tell.
    ///
    /// This is the maximum of:
    /// - the `start_height` from the peer's version message, representing the
    ///   last block the peer had when the connection was established. It may be
    ///   stale for long-lived connections, but during initial sync it provides
    ///   a useful lower bound for filtering out peers that are behind; and
    /// - the live height: the highest block height this peer has demonstrably
    ///   served us. This is a trusted direct signal, raised only by blocks the
    ///   peer actually delivered, never by gossip.
    pub fn remote_height(&self) -> Height {
        let live_height = Height(self.live_height.load(Ordering::Relaxed));

        std::cmp::max(self.connection_info.remote.start_height, live_height)
    }

    /// Returns a handle to this peer's live height, so a response future can
    /// record delivered block heights after the request completes.
    ///
    /// To keep the live height monotonic, holders must only raise it using
    /// [`fetch_max`](AtomicU32::fetch_max).
    pub(crate) fn live_height_handle(&self) -> Arc<AtomicU32> {
        self.live_height.clone()
    }

    /// Returns the number of blocks downloaded from this peer over the
    /// connection's lifetime.
    pub fn blocks_received(&self) -> u64 {
        self.blocks_received.load(Ordering::Relaxed)
    }

    /// Returns a handle to this peer's block-download counter, so a response
    /// future can record blocks received after the request completes.
    pub(crate) fn blocks_received_handle(&self) -> Arc<AtomicU64> {
        self.blocks_received.clone()
    }
}

impl<Request> Service<Request> for LoadTrackedClient
where
    Client: Service<Request>,
{
    type Response = <Client as Service<Request>>::Response;
    type Error = <Client as Service<Request>>::Error;
    type Future = <PeakEwma<Client> as Service<Request>>::Future;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(context)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.service.call(request)
    }
}

impl Load for LoadTrackedClient {
    type Metric = <PeakEwma<Client> as Load>::Metric;

    fn load(&self) -> Self::Metric {
        self.service.load()
    }
}
