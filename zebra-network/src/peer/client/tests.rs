//! Tests for the [`Client`] part of peer connections, and some test utilities for mocking
//! [`Client`] instances.

mod vectors;

use std::time::Duration;

use futures::channel::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::{
    peer::{error::SharedPeerError, CancelHeartbeatTask, Client, ClientRequest, ErrorSlot},
    protocol::external::types::Version,
};

/// The maximum time a mocked peer connection should be alive during a test.
const MAX_PEER_CONNECTION_TIME: Duration = Duration::from_secs(10);

/// A harness with mocked channels for testing a [`Client`] instance.
pub struct ClientTestHarness {
    client_request_receiver: Option<mpsc::Receiver<ClientRequest>>,
    shutdown_receiver: Option<oneshot::Receiver<CancelHeartbeatTask>>,
    error_slot: ErrorSlot,
    version: Version,
}

impl ClientTestHarness {
    /// Create a [`ClientTestHarnessBuilder`] instance to help create a new [`Client`] instance
    /// and a [`ClientTestHarness`] to track it.
    pub fn build() -> ClientTestHarnessBuilder {
        ClientTestHarnessBuilder { version: None }
    }

    /// Gets the peer protocol version associated to the [`Client`].
    pub fn version(&self) -> Version {
        self.version
    }

    /// Returns true if the [`Client`] instance still wants connection heartbeats to be sent.
    ///
    /// Checks that the client:
    /// - has not been dropped,
    /// - has not closed or dropped the mocked heartbeat task channel, and
    /// - has not asked the mocked heartbeat task to shut down.
    pub fn wants_connection_heartbeats(&mut self) -> bool {
        let receive_result = self
            .shutdown_receiver
            .as_mut()
            .expect("heartbeat shutdown receiver endpoint has been dropped")
            .try_recv();

        match receive_result {
            Ok(None) => true,
            Ok(Some(CancelHeartbeatTask)) | Err(oneshot::Canceled) => false,
        }
    }

    /// Drops the mocked heartbeat shutdown receiver endpoint.
    pub fn drop_heartbeat_shutdown_receiver(&mut self) {
        let _ = self
            .shutdown_receiver
            .take()
            .expect("heartbeat shutdown receiver endpoint has already been dropped");
    }

    /// Closes the receiver endpoint of [`ClientRequests`] that are supposed to be sent to the
    /// remote peer.
    ///
    /// The remote peer that would receive the requests is mocked for testing.
    pub fn close_outbound_client_request_receiver(&mut self) {
        self.client_request_receiver
            .as_mut()
            .expect("request receiver endpoint has been dropped")
            .close();
    }

    /// Drops the receiver endpoint of [`ClientRequests`], forcefully closing the channel.
    ///
    /// The remote peer that would receive the requests is mocked for testing.
    pub fn drop_outbound_client_request_receiver(&mut self) {
        self.client_request_receiver
            .take()
            .expect("request receiver endpoint has already been dropped");
    }

    /// Tries to receive a [`ClientRequest`] sent by the [`Client`] instance.
    ///
    /// The remote peer that would receive the requests is mocked for testing.
    pub(crate) fn try_to_receive_outbound_client_request(&mut self) -> ReceiveRequestAttempt {
        let receive_result = self
            .client_request_receiver
            .as_mut()
            .expect("request receiver endpoint has been dropped")
            .try_next();

        match receive_result {
            Ok(Some(request)) => ReceiveRequestAttempt::Request(request),
            Ok(None) => ReceiveRequestAttempt::Closed,
            Err(_) => ReceiveRequestAttempt::Empty,
        }
    }

    /// Returns the current error in the [`ErrorSlot`], if there is one.
    pub fn current_error(&self) -> Option<SharedPeerError> {
        self.error_slot.try_get_error()
    }

    /// Sets the error in the [`ErrorSlot`], assuming there isn't one already.
    ///
    /// # Panics
    ///
    /// If there's already an error in the [`ErrorSlot`].
    pub fn set_error(&self, error: impl Into<SharedPeerError>) {
        self.error_slot
            .try_update_error(error.into())
            .expect("unexpected earlier error in error slot")
    }
}

/// The result of an attempt to receive a [`ClientRequest`] sent by the [`Client`] instance.
///
/// The remote peer that would receive the request is mocked for testing.
pub(crate) enum ReceiveRequestAttempt {
    /// The [`Client`] instance has closed the sender endpoint of the channel.
    Closed,

    /// There were no queued requests in the channel.
    Empty,

    /// One request was successfully received.
    Request(ClientRequest),
}

impl ReceiveRequestAttempt {
    /// Check if the attempt to receive resulted in discovering that the sender endpoint had been
    /// closed.
    pub fn is_closed(&self) -> bool {
        matches!(self, ReceiveRequestAttempt::Closed)
    }

    /// Check if the attempt to receive resulted in no requests.
    pub fn is_empty(&self) -> bool {
        matches!(self, ReceiveRequestAttempt::Empty)
    }

    /// Returns the received request, if there was one.
    #[allow(dead_code)]
    pub fn request(self) -> Option<ClientRequest> {
        match self {
            ReceiveRequestAttempt::Request(request) => Some(request),
            ReceiveRequestAttempt::Closed | ReceiveRequestAttempt::Empty => None,
        }
    }
}

/// A builder for a [`Client`] and [`ClientTestHarness`] instance.
///
/// Mocked data is used to construct a real [`Client`] instance. The mocked data is initialized by
/// the [`ClientTestHarnessBuilder`], and can be accessed and changed through the
/// [`ClientTestHarness`].
pub struct ClientTestHarnessBuilder {
    version: Option<Version>,
}

impl ClientTestHarnessBuilder {
    /// Configure the mocked version for the peer.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Build a [`Client`] instance with the mocked data and a [`ClientTestHarness`] to track it.
    pub fn finish(self) -> (Client, ClientTestHarness) {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (client_request_sender, client_request_receiver) = mpsc::channel(1);
        let error_slot = ErrorSlot::default();
        let version = self.version.unwrap_or(Version(0));

        let client = Client {
            shutdown_tx: Some(shutdown_sender),
            server_tx: client_request_sender,
            error_slot: error_slot.clone(),
            version,
            connection_task: Self::mock_background_task(),
            heartbeat_task: Self::mock_background_task(),
        };

        let harness = ClientTestHarness {
            client_request_receiver: Some(client_request_receiver),
            shutdown_receiver: Some(shutdown_receiver),
            error_slot,
            version,
        };

        (client, harness)
    }

    /// Spawn a dummy background task.
    ///
    /// The task lives as long as [`MAX_PEER_CONNECTION_TIME`].
    fn mock_background_task() -> JoinHandle<()> {
        tokio::spawn(tokio::time::sleep(MAX_PEER_CONNECTION_TIME))
    }
}
