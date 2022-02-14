//! Tests for the [`Client`] part of peer connections, and some test utilities for mocking
//! [`Client`] instances.

mod vectors;

use std::time::Duration;

use futures::{
    channel::{mpsc, oneshot},
    future::{self, AbortHandle, Future, FutureExt},
};
use tokio::{
    sync::broadcast::{self, error::TryRecvError},
    task::JoinHandle,
};

use crate::{
    peer::{error::SharedPeerError, CancelHeartbeatTask, Client, ClientRequest, ErrorSlot},
    peer_set::InventoryChange,
    protocol::external::types::Version,
};

/// The maximum time a mocked peer connection should be alive during a test.
const MAX_PEER_CONNECTION_TIME: Duration = Duration::from_secs(10);

/// A harness with mocked channels for testing a [`Client`] instance.
pub struct ClientTestHarness {
    client_request_receiver: Option<mpsc::Receiver<ClientRequest>>,
    shutdown_receiver: Option<oneshot::Receiver<CancelHeartbeatTask>>,
    #[allow(dead_code)]
    inv_receiver: Option<broadcast::Receiver<InventoryChange>>,
    error_slot: ErrorSlot,
    version: Version,
    connection_aborter: AbortHandle,
    heartbeat_aborter: AbortHandle,
}

impl ClientTestHarness {
    /// Create a [`ClientTestHarnessBuilder`] instance to help create a new [`Client`] instance
    /// and a [`ClientTestHarness`] to track it.
    pub fn build() -> ClientTestHarnessBuilder {
        ClientTestHarnessBuilder {
            version: None,
            connection_task: None,
            heartbeat_task: None,
        }
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

    /// Drops the receiver endpoint of [`InventoryChanges`], forcefully closing the channel.
    ///
    /// The inventory registry that would track the changes is mocked for testing.
    ///
    /// Note: this closes the broadcast receiver, it doesn't have a separate `close()` method.
    #[allow(dead_code)]
    pub fn drop_inventory_change_receiver(&mut self) {
        self.inv_receiver
            .take()
            .expect("inventory change receiver endpoint has already been dropped");
    }

    /// Tries to receive an [`InventoryChange`] sent by the [`Client`] instance.
    ///
    /// This method acts like a mock inventory registry, allowing tests to track the changes.
    ///
    /// TODO: make ReceiveRequestAttempt generic, and use it here.
    #[allow(dead_code)]
    pub(crate) fn try_to_receive_inventory_change(&mut self) -> Option<InventoryChange> {
        let receive_result = self
            .inv_receiver
            .as_mut()
            .expect("inventory change receiver endpoint has been dropped")
            .try_recv();

        match receive_result {
            Ok(change) => Some(change),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Closed) => None,
            Err(TryRecvError::Lagged(skipped_messages)) => unreachable!(
                "unexpected lagged inventory receiver in tests, skipped {} messages",
                skipped_messages,
            ),
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

    /// Stops the mock background task that handles incoming remote requests and replies.
    pub async fn stop_connection_task(&self) {
        self.connection_aborter.abort();

        // Allow the task to detect that it was aborted.
        tokio::task::yield_now().await;
    }

    /// Stops the mock background task that sends periodic heartbeats.
    pub async fn stop_heartbeat_task(&self) {
        self.heartbeat_aborter.abort();

        // Allow the task to detect that it was aborted.
        tokio::task::yield_now().await;
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
pub struct ClientTestHarnessBuilder<C = future::Ready<()>, H = future::Ready<()>> {
    connection_task: Option<C>,
    heartbeat_task: Option<H>,
    version: Option<Version>,
}

impl<C, H> ClientTestHarnessBuilder<C, H>
where
    C: Future<Output = ()> + Send + 'static,
    H: Future<Output = ()> + Send + 'static,
{
    /// Configure the mocked version for the peer.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Configure the mock connection task future to use.
    pub fn with_connection_task<NewC>(
        self,
        connection_task: NewC,
    ) -> ClientTestHarnessBuilder<NewC, H> {
        ClientTestHarnessBuilder {
            connection_task: Some(connection_task),
            heartbeat_task: self.heartbeat_task,
            version: self.version,
        }
    }

    /// Configure the mock heartbeat task future to use.
    pub fn with_heartbeat_task<NewH>(
        self,
        heartbeat_task: NewH,
    ) -> ClientTestHarnessBuilder<C, NewH> {
        ClientTestHarnessBuilder {
            connection_task: self.connection_task,
            heartbeat_task: Some(heartbeat_task),
            version: self.version,
        }
    }

    /// Build a [`Client`] instance with the mocked data and a [`ClientTestHarness`] to track it.
    pub fn finish(self) -> (Client, ClientTestHarness) {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (client_request_sender, client_request_receiver) = mpsc::channel(1);
        let (inv_sender, inv_receiver) = broadcast::channel(5);

        let error_slot = ErrorSlot::default();
        let version = self.version.unwrap_or(Version(0));

        let (connection_task, connection_aborter) =
            Self::spawn_background_task_or_fallback(self.connection_task);
        let (heartbeat_task, heartbeat_aborter) =
            Self::spawn_background_task_or_fallback(self.heartbeat_task);

        let client = Client {
            shutdown_tx: Some(shutdown_sender),
            server_tx: client_request_sender,
            inv_collector: inv_sender,
            transient_addr: None,
            error_slot: error_slot.clone(),
            version,
            connection_task,
            heartbeat_task,
        };

        let harness = ClientTestHarness {
            client_request_receiver: Some(client_request_receiver),
            shutdown_receiver: Some(shutdown_receiver),
            inv_receiver: Some(inv_receiver),
            error_slot,
            version,
            connection_aborter,
            heartbeat_aborter,
        };

        (client, harness)
    }

    /// Spawn a mock background abortable task `task_future` if provided, or a fallback task
    /// otherwise.
    ///
    /// The fallback task lives as long as [`MAX_PEER_CONNECTION_TIME`].
    fn spawn_background_task_or_fallback<T>(task_future: Option<T>) -> (JoinHandle<()>, AbortHandle)
    where
        T: Future<Output = ()> + Send + 'static,
    {
        match task_future {
            Some(future) => Self::spawn_background_task(future),
            None => Self::spawn_background_task(tokio::time::sleep(MAX_PEER_CONNECTION_TIME)),
        }
    }

    /// Spawn a mock background abortable task to run `task_future`.
    fn spawn_background_task<T>(task_future: T) -> (JoinHandle<()>, AbortHandle)
    where
        T: Future<Output = ()> + Send + 'static,
    {
        let (task, abort_handle) = future::abortable(task_future);
        let task_handle = tokio::spawn(task.map(|_result| ()));

        (task_handle, abort_handle)
    }
}
