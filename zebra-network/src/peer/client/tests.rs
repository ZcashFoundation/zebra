//! Tests for the [`Client`] part of peer connections, and some test utilities for mocking
//! [`Client`] instances.

mod vectors;

use futures::channel::{mpsc, oneshot};

use crate::{
    peer::{CancelHeartbeatTask, Client, ClientRequest, ErrorSlot, LoadTrackedClient},
    protocol::external::types::Version,
};

/// A handle to a mocked [`Client`] instance.
pub struct MockedClientHandle {
    _request_receiver: mpsc::Receiver<ClientRequest>,
    shutdown_receiver: oneshot::Receiver<CancelHeartbeatTask>,
    version: Version,
}

impl MockedClientHandle {
    /// Create a new mocked [`Client`] instance, returning it together with a handle to track it.
    pub fn new(version: Version) -> (Self, LoadTrackedClient) {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (request_sender, _request_receiver) = mpsc::channel(1);

        let client = Client {
            shutdown_tx: Some(shutdown_sender),
            server_tx: request_sender,
            error_slot: ErrorSlot::default(),
            version,
        };

        let handle = MockedClientHandle {
            _request_receiver,
            shutdown_receiver,
            version,
        };

        (handle, client.into())
    }

    /// Gets the peer protocol version associated to the [`Client`].
    pub fn version(&self) -> Version {
        self.version
    }

    /// Checks if the [`Client`] instance has not been dropped, which would have disconnected from
    /// the peer.
    pub fn is_connected(&mut self) -> bool {
        match self.shutdown_receiver.try_recv() {
            Ok(None) => true,
            Ok(Some(CancelHeartbeatTask)) | Err(oneshot::Canceled) => false,
        }
    }
}
