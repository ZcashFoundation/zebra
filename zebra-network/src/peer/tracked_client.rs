//! A peer service wrapper for load measurements, protocol metadata, and connection cleanup.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    task::{Context, Poll},
};

use tower::{
    load::{Load, PeakEwma},
    Service,
};

use crate::{
    constants::{EWMA_DECAY_TIME_NANOS, EWMA_DEFAULT_RTT},
    peer::{Client, ConnectedAddr, ConnectionInfo},
    peer_set::ConnectionGuard,
    protocol::external::{canonical_socket_addr, types::Version},
};

/// A client service wrapper that keeps track of its load.
///
/// It also tracks the peer's reported protocol version and, once admitted to a
/// peer set, reports connection closure when the service is dropped.
#[derive(Debug)]
pub struct TrackedClient {
    /// A service representing a connected peer, wrapped in a load tracker.
    service: PeakEwma<Client>,

    /// The metadata for the connected peer `service`.
    connection_info: Arc<ConnectionInfo>,

    /// Owned only by this service, never by its response futures or shared metadata.
    connection_guard: Option<ConnectionGuard>,
}

/// Create a new [`TrackedClient`] wrapping the provided `client` service.
impl From<Client> for TrackedClient {
    fn from(client: Client) -> Self {
        let connection_info = client.connection_info.clone();

        let service = PeakEwma::new(
            client,
            EWMA_DEFAULT_RTT,
            EWMA_DECAY_TIME_NANOS,
            tower::load::CompleteOnResponse::default(),
        );

        TrackedClient {
            service,
            connection_info,
            connection_guard: None,
        }
    }
}

impl TrackedClient {
    /// Attaches the cleanup guard when this connection is admitted to a peer set.
    pub(crate) fn track_connection(&mut self, guard: ConnectionGuard) {
        assert!(
            self.connection_guard.is_none(),
            "a connection is admitted only once"
        );
        self.connection_guard = Some(guard);
    }

    /// Retrieve the peer's reported protocol version.
    pub fn remote_version(&self) -> Version {
        self.connection_info.remote.version
    }

    /// Returns true if this peer connected directly to us from `ip`.
    pub fn is_inbound_direct_from_ip(&self, ip: &IpAddr) -> bool {
        let expected_ip = canonical_socket_addr(SocketAddr::new(*ip, 0)).ip();

        matches!(
            self.connection_info.connected_addr,
            ConnectedAddr::InboundDirect { addr }
                if canonical_socket_addr(addr.remove_socket_addr_privacy()).ip() == expected_ip
        )
    }
}

impl<Request> Service<Request> for TrackedClient
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

impl Load for TrackedClient {
    type Metric = <PeakEwma<Client> as Load>::Metric;

    fn load(&self) -> Self::Metric {
        self.service.load()
    }
}
