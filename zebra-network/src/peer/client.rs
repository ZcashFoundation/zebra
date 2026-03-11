//! Handles outbound requests from our node to the network.

use std::{
    collections::HashSet,
    future::Future,
    iter,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    channel::{mpsc, oneshot},
    future, ready,
    stream::{Stream, StreamExt},
    FutureExt,
};
use tokio::{sync::broadcast, task::JoinHandle};
use tower::Service;

use zebra_chain::diagnostic::task::CheckForPanics;

use crate::{
    peer::{
        error::{AlreadyErrored, ErrorSlot, PeerError, SharedPeerError},
        ConnectionInfo,
    },
    peer_set::InventoryChange,
    protocol::{
        external::InventoryHash,
        internal::{Request, Response},
    },
    BoxError, PeerSocketAddr,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

/// The "client" duplex half of a peer connection.
pub struct Client {
    /// The metadata for the connected peer `service`.
    pub connection_info: Arc<ConnectionInfo>,

    /// Used to shut down the corresponding heartbeat.
    /// This is always Some except when we take it on drop.
    pub(crate) shutdown_tx: Option<oneshot::Sender<CancelHeartbeatTask>>,

    /// Used to send [`Request`]s to the remote peer.
    pub(crate) server_tx: mpsc::Sender<ClientRequest>,

    /// Used to register missing inventory in client [`Response`]s,
    /// so that the peer set can route retries to other clients.
    pub(crate) inv_collector: broadcast::Sender<InventoryChange>,

    /// A slot for an error shared between the Connection and the Client that uses it.
    ///
    /// `None` unless the connection or client have errored.
    pub(crate) error_slot: ErrorSlot,

    /// A handle to the task responsible for connecting to the peer.
    pub(crate) connection_task: JoinHandle<()>,

    /// A handle to the task responsible for sending periodic heartbeats.
    pub(crate) heartbeat_task: JoinHandle<Result<(), BoxError>>,
}

/// A signal sent by the [`Client`] half of a peer connection,
/// to cancel a [`Client`]'s heartbeat task.
///
/// When it receives this signal, the heartbeat task exits.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CancelHeartbeatTask;

/// A message from the `peer::Client` to the `peer::Server`.
#[derive(Debug)]
pub(crate) struct ClientRequest {
    /// The actual network request for the peer.
    pub request: Request,

    /// The response `Message` channel, included because `peer::Client::call`
    /// returns a future that may be moved around before it resolves.
    pub tx: oneshot::Sender<Result<Response, SharedPeerError>>,

    /// Used to register missing inventory in responses on `tx`,
    /// so that the peer set can route retries to other clients.
    pub inv_collector: Option<broadcast::Sender<InventoryChange>>,

    /// The peer address for registering missing inventory.
    ///
    /// TODO: replace this with `ConnectedAddr`?
    pub transient_addr: Option<PeerSocketAddr>,

    /// The tracing context for the request, so that work the connection task does
    /// processing messages in the context of this request will have correct context.
    pub span: tracing::Span,
}

/// A receiver for the `peer::Server`, which wraps a `mpsc::Receiver`,
/// converting `ClientRequest`s into `InProgressClientRequest`s.
#[derive(Debug)]
pub(super) struct ClientRequestReceiver {
    /// The inner receiver
    inner: mpsc::Receiver<ClientRequest>,
}

/// A message from the `peer::Client` to the `peer::Server`,
/// after it has been received by the `peer::Server`.
#[derive(Debug)]
#[must_use = "tx.send() must be called before drop"]
pub(super) struct InProgressClientRequest {
    /// The actual request.
    pub request: Request,

    /// The return message channel, included because `peer::Client::call` returns a
    /// future that may be moved around before it resolves.
    ///
    /// INVARIANT: `tx.send()` must be called before dropping `tx`.
    ///
    /// JUSTIFICATION: the `peer::Client` translates `Request`s into
    /// `ClientRequest`s, which it sends to a background task. If the send is
    /// `Ok(())`, it will assume that it is safe to unconditionally poll the
    /// `Receiver` tied to the `Sender` used to create the `ClientRequest`.
    ///
    /// We also take advantage of this invariant to route inventory requests
    /// away from peers that did not respond with that inventory.
    ///
    /// We enforce this invariant via the type system, by converting
    /// `ClientRequest`s to `InProgressClientRequest`s when they are received by
    /// the background task. These conversions are implemented by
    /// `ClientRequestReceiver`.
    pub tx: MustUseClientResponseSender,

    /// The tracing context for the request, so that work the connection task does
    /// processing messages in the context of this request will have correct context.
    pub span: tracing::Span,
}

/// A `oneshot::Sender` for client responses, that must be used by calling `send()`.
/// Also handles forwarding missing inventory to the inventory registry.
///
/// Panics on drop if `tx` has not been used or canceled.
/// Panics if `tx.send()` is used more than once.
#[derive(Debug)]
#[must_use = "tx.send() must be called before drop"]
pub(super) struct MustUseClientResponseSender {
    /// The sender for the oneshot client response channel.
    ///
    /// `None` if `tx.send()` has been used.
    pub tx: Option<oneshot::Sender<Result<Response, SharedPeerError>>>,

    /// Forwards missing inventory in the response to the inventory collector.
    ///
    /// Boxed to reduce the size of containing structures.
    pub missing_inv: Option<Box<MissingInventoryCollector>>,
}

/// Forwards missing inventory in the response to the inventory registry.
#[derive(Debug)]
pub(super) struct MissingInventoryCollector {
    /// A clone of the original request, if it is an inventory request.
    ///
    /// This struct is only ever created with inventory requests.
    request: Request,

    /// Used to register missing inventory from responses,
    /// so that the peer set can route retries to other clients.
    collector: broadcast::Sender<InventoryChange>,

    /// The peer address for registering missing inventory.
    transient_addr: PeerSocketAddr,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // skip the channels, they don't tell us anything useful
        f.debug_struct("Client")
            .field("connection_info", &self.connection_info)
            .field("error_slot", &self.error_slot)
            .field("connection_task", &self.connection_task)
            .field("heartbeat_task", &self.heartbeat_task)
            .finish()
    }
}

impl From<ClientRequest> for InProgressClientRequest {
    fn from(client_request: ClientRequest) -> Self {
        let ClientRequest {
            request,
            tx,
            inv_collector,
            transient_addr,
            span,
        } = client_request;

        let tx = MustUseClientResponseSender::new(tx, &request, inv_collector, transient_addr);

        InProgressClientRequest { request, tx, span }
    }
}

impl ClientRequestReceiver {
    /// Forwards to `inner.close()`.
    pub fn close(&mut self) {
        self.inner.close()
    }

    /// Closes `inner`, then gets the next pending [`Request`].
    ///
    /// Closing the channel ensures that:
    /// - the request stream terminates, and
    /// - task notifications are not required.
    pub fn close_and_flush_next(&mut self) -> Option<InProgressClientRequest> {
        self.inner.close();

        // # Correctness
        //
        // The request stream terminates, because the sender is closed,
        // and the channel has a limited capacity.
        // Task notifications are not required, because the sender is closed.
        //
        // Despite what its documentation says, we've seen futures::channel::mpsc::Receiver::try_next()
        // return an error after the channel is closed.
        self.inner.try_next().ok()?.map(Into::into)
    }
}

impl Stream for ClientRequestReceiver {
    type Item = InProgressClientRequest;

    /// Converts the successful result of `inner.poll_next()` to an
    /// `InProgressClientRequest`.
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Ready(client_request) => Poll::Ready(client_request.map(Into::into)),
            // CORRECTNESS
            //
            // The current task must be scheduled for wakeup every time we
            // return `Poll::Pending`.
            //
            // inner.poll_next_unpin` schedules this task for wakeup when
            // there are new items available in the inner stream.
            Poll::Pending => Poll::Pending,
        }
    }

    /// Returns `inner.size_hint()`
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl From<mpsc::Receiver<ClientRequest>> for ClientRequestReceiver {
    fn from(rx: mpsc::Receiver<ClientRequest>) -> Self {
        ClientRequestReceiver { inner: rx }
    }
}

impl MustUseClientResponseSender {
    /// Returns a newly created client response sender for `tx`.
    ///
    /// If `request` or the response contains missing inventory,
    /// it is forwarded to the `inv_collector`, for the peer at `transient_addr`.
    pub fn new(
        tx: oneshot::Sender<Result<Response, SharedPeerError>>,
        request: &Request,
        inv_collector: Option<broadcast::Sender<InventoryChange>>,
        transient_addr: Option<PeerSocketAddr>,
    ) -> Self {
        Self {
            tx: Some(tx),
            missing_inv: MissingInventoryCollector::new(request, inv_collector, transient_addr),
        }
    }

    /// Forwards `response` to `tx.send()`, and missing inventory to `inv_collector`,
    /// and marks this sender as used.
    ///
    /// Panics if `tx.send()` is used more than once.
    pub fn send(
        mut self,
        response: Result<Response, SharedPeerError>,
    ) -> Result<(), Result<Response, SharedPeerError>> {
        // Forward any missing inventory to the registry.
        if let Some(missing_inv) = self.missing_inv.take() {
            missing_inv.send(&response);
        }

        // Forward the response to the internal requester.
        self.tx
            .take()
            .unwrap_or_else(|| {
                panic!(
                    "multiple uses of response sender: response must be sent exactly once: {self:?}"
                )
            })
            .send(response)
    }

    /// Returns `tx.cancellation()`.
    ///
    /// Panics if `tx.send()` has previously been used.
    pub fn cancellation(&mut self) -> oneshot::Cancellation<'_, Result<Response, SharedPeerError>> {
        self.tx
            .as_mut()
            .map(|tx| tx.cancellation())
            .unwrap_or_else( || {
                panic!("called cancellation() after using oneshot sender: oneshot must be used exactly once")
            })
    }

    /// Returns `tx.is_canceled()`.
    ///
    /// Panics if `tx.send()` has previously been used.
    pub fn is_canceled(&self) -> bool {
        self.tx
            .as_ref()
            .map(|tx| tx.is_canceled())
            .unwrap_or_else(
                || panic!("called is_canceled() after using oneshot sender: oneshot must be used exactly once: {self:?}"))
    }
}

impl Drop for MustUseClientResponseSender {
    #[instrument(skip(self))]
    fn drop(&mut self) {
        // we don't panic if we are shutting down anyway
        if !zebra_chain::shutdown::is_shutting_down() {
            // is_canceled() will not panic, because we check is_none() first
            assert!(
                self.tx.is_none() || self.is_canceled(),
                "unused client response sender: oneshot must be used or canceled: {self:?}"
            );
        }
    }
}

impl MissingInventoryCollector {
    /// Returns a newly created missing inventory collector, if needed.
    ///
    /// If `request` or the response contains missing inventory,
    /// it is forwarded to the `inv_collector`, for the peer at `transient_addr`.
    pub fn new(
        request: &Request,
        inv_collector: Option<broadcast::Sender<InventoryChange>>,
        transient_addr: Option<PeerSocketAddr>,
    ) -> Option<Box<MissingInventoryCollector>> {
        if !request.is_inventory_download() {
            return None;
        }

        if let (Some(inv_collector), Some(transient_addr)) = (inv_collector, transient_addr) {
            Some(Box::new(MissingInventoryCollector {
                request: request.clone(),
                collector: inv_collector,
                transient_addr,
            }))
        } else {
            None
        }
    }

    /// Forwards any missing inventory to the registry.
    ///
    /// `zcashd` doesn't send `notfound` messages for blocks,
    /// so we need to track missing blocks ourselves.
    ///
    /// This can sometimes send duplicate missing inventory,
    /// but the registry ignores duplicates anyway.
    pub fn send(self, response: &Result<Response, SharedPeerError>) {
        let missing_inv: HashSet<InventoryHash> = match (self.request, response) {
            // Missing block hashes from partial responses.
            (_, Ok(Response::Blocks(block_statuses))) => block_statuses
                .iter()
                .filter_map(|b| b.missing())
                .map(InventoryHash::Block)
                .collect(),

            // Missing transaction IDs from partial responses.
            (_, Ok(Response::Transactions(tx_statuses))) => tx_statuses
                .iter()
                .filter_map(|tx| tx.missing())
                .map(|tx| tx.into())
                .collect(),

            // Other response types never contain missing inventory.
            (_, Ok(_)) => iter::empty().collect(),

            // We don't forward NotFoundRegistry errors,
            // because the errors are generated locally from the registry,
            // so those statuses are already in the registry.
            //
            // Unfortunately, we can't access the inner error variant here,
            // due to TracedError.
            (_, Err(e)) if e.inner_debug().contains("NotFoundRegistry") => iter::empty().collect(),

            // Missing inventory from other errors, including NotFoundResponse, timeouts,
            // and dropped connections.
            (request, Err(_)) => {
                // The request either contains blocks or transactions,
                // but this is a convenient way to collect them both.
                let missing_blocks = request
                    .block_hash_inventory()
                    .into_iter()
                    .map(InventoryHash::Block);

                let missing_txs = request
                    .transaction_id_inventory()
                    .into_iter()
                    .map(InventoryHash::from);

                missing_blocks.chain(missing_txs).collect()
            }
        };

        if let Some(missing_inv) =
            InventoryChange::new_missing_multi(missing_inv.iter(), self.transient_addr)
        {
            // if all the receivers are closed, assume we're in tests or an isolated connection
            let _ = self.collector.send(missing_inv);
        }
    }
}

impl Client {
    /// Check if this connection's heartbeat task has exited.
    ///
    /// Returns an error if the heartbeat task exited. Otherwise, schedules the client task for
    /// wakeup when the heartbeat task finishes, or the channel closes, and returns `Pending`.
    ///
    /// # Panics
    ///
    /// If the heartbeat task panicked.
    #[allow(clippy::unwrap_in_result)]
    fn poll_heartbeat(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SharedPeerError>> {
        let is_canceled = self
            .shutdown_tx
            .as_mut()
            .expect("only taken on drop")
            .poll_canceled(cx)
            .is_ready();

        let result = match self.heartbeat_task.poll_unpin(cx) {
            Poll::Pending => {
                // The heartbeat task returns `Pending` while it continues to run.
                // But if it has dropped its receiver, it is shutting down, and we should also shut down.
                if is_canceled {
                    self.set_task_exited_error(
                        "heartbeat",
                        PeerError::HeartbeatTaskExited("Task was cancelled".to_string()),
                    )
                } else {
                    // Heartbeat task is still running.
                    return Poll::Pending;
                }
            }
            Poll::Ready(Ok(Ok(_))) => {
                // Heartbeat task stopped unexpectedly, without panic or error.
                self.set_task_exited_error(
                    "heartbeat",
                    PeerError::HeartbeatTaskExited(
                        "Heartbeat task stopped unexpectedly".to_string(),
                    ),
                )
            }
            Poll::Ready(Ok(Err(error))) => {
                // Heartbeat task stopped unexpectedly, with error.
                self.set_task_exited_error(
                    "heartbeat",
                    PeerError::HeartbeatTaskExited(error.to_string()),
                )
            }
            Poll::Ready(Err(error)) => {
                // Heartbeat task panicked.
                let error = error.panic_if_task_has_panicked();

                // Heartbeat task was cancelled.
                if error.is_cancelled() {
                    self.set_task_exited_error(
                        "heartbeat",
                        PeerError::HeartbeatTaskExited("Task was cancelled".to_string()),
                    )
                }
                // Heartbeat task stopped with another kind of task error.
                else {
                    self.set_task_exited_error(
                        "heartbeat",
                        PeerError::HeartbeatTaskExited(error.to_string()),
                    )
                }
            }
        };

        Poll::Ready(result)
    }

    /// Check if the connection's request/response task has exited.
    ///
    /// Returns an error if the connection task exited. Otherwise, schedules the client task for
    /// wakeup when the connection task finishes, and returns `Pending`.
    ///
    /// # Panics
    ///
    /// If the connection task panicked.
    fn poll_connection(&mut self, context: &mut Context<'_>) -> Poll<Result<(), SharedPeerError>> {
        // Return `Pending` if the connection task is still running.
        let result = match ready!(self.connection_task.poll_unpin(context)) {
            Ok(()) => {
                // Connection task stopped unexpectedly, without panicking.
                self.set_task_exited_error("connection", PeerError::ConnectionTaskExited)
            }
            Err(error) => {
                // Connection task panicked or was cancelled.
                let _ = error.panic_if_task_has_panicked();

                // Any unexpected termination of the connection task is reported as ConnectionTaskExited.
                self.set_task_exited_error("connection", PeerError::ConnectionTaskExited)
            }
        };

        Poll::Ready(result)
    }

    /// Properly update the error slot after a background task has unexpectedly stopped.
    fn set_task_exited_error(
        &mut self,
        task_name: &str,
        error: PeerError,
    ) -> Result<(), SharedPeerError> {
        // Make sure there is an error in the slot
        let task_error = SharedPeerError::from(error);
        let original_error = self.error_slot.try_update_error(task_error.clone());
        debug!(
            ?original_error,
            latest_error = ?task_error,
            "client {} task exited", task_name
        );

        if let Err(AlreadyErrored { original_error }) = original_error {
            Err(original_error)
        } else {
            Err(task_error)
        }
    }

    /// Poll for space in the shared request sender channel.
    ///
    /// Returns an error if the sender channel is closed. If there is no space in the channel,
    /// returns `Pending`, and schedules the task for wakeup when there is space available.
    fn poll_request(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SharedPeerError>> {
        let server_result = ready!(self.server_tx.poll_ready(cx));
        if server_result.is_err() {
            Poll::Ready(Err(self
                .error_slot
                .try_get_error()
                .unwrap_or_else(|| PeerError::ConnectionTaskExited.into())))
        } else if let Some(error) = self.error_slot.try_get_error() {
            Poll::Ready(Err(error))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    /// Poll for space in the shared request sender channel, and for errors in the connection tasks.
    ///
    /// Returns an error if the sender channel is closed, or the heartbeat or connection tasks have
    /// terminated. If there is no space in the channel, returns `Pending`, and schedules the task
    /// for wakeup when there is space available, or one of the tasks terminates.
    fn poll_client(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SharedPeerError>> {
        // # Correctness
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // `poll_heartbeat()` and `poll_connection()` schedule the client task for wakeup
        // if either task exits, or if the heartbeat task drops the cancel handle.
        //
        //`ready!` returns `Poll::Pending` when `server_tx` is unready, and
        // schedules this task for wakeup.
        //
        // It's ok to exit early and skip wakeups when there is an error, because the connection
        // and its tasks are shut down immediately on error.

        let _heartbeat_pending: Poll<()> = self.poll_heartbeat(cx)?;
        let _connection_pending: Poll<()> = self.poll_connection(cx)?;

        // We're only pending if the sender channel is full.
        self.poll_request(cx)
    }

    /// Shut down the resources held by the client half of this peer connection.
    ///
    /// Stops further requests to the remote peer, and stops the heartbeat task.
    fn shutdown(&mut self) {
        // Prevent any senders from sending more messages to this peer.
        self.server_tx.close_channel();

        // Ask the heartbeat task to stop.
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(CancelHeartbeatTask);
        }

        // Force the connection and heartbeat tasks to stop.
        self.connection_task.abort();
        self.heartbeat_task.abort();
    }
}

impl Service<Request> for Client {
    type Response = Response;
    type Error = SharedPeerError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // # Correctness
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // `poll_client()` schedules the client task for wakeup if the sender channel has space,
        // either connection task exits, or if the heartbeat task drops the cancel handle.

        // Check all the tasks and channels.
        //
        //`ready!` returns `Poll::Pending` when `server_tx` is unready, and
        // schedules this task for wakeup.
        let result = ready!(self.poll_client(cx));

        // Shut down the client and connection if there is an error.
        if let Err(error) = result {
            self.shutdown();

            Poll::Ready(Err(error))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let (tx, rx) = oneshot::channel();
        // get the current Span to propagate it to the peer connection task.
        // this allows the peer connection to enter the correct tracing context
        // when it's handling messages in the context of processing this
        // request.
        let span = tracing::Span::current();

        match self.server_tx.try_send(ClientRequest {
            request,
            tx,
            inv_collector: Some(self.inv_collector.clone()),
            transient_addr: self.connection_info.connected_addr.get_transient_addr(),
            span,
        }) {
            Err(e) => {
                if e.is_disconnected() {
                    let peer_error = self
                        .error_slot
                        .try_get_error()
                        .unwrap_or_else(|| PeerError::ConnectionTaskExited.into());

                    let ClientRequest { tx, .. } = e.into_inner();
                    let _ = tx.send(Err(peer_error.clone()));

                    future::ready(Err(peer_error)).boxed()
                } else {
                    // sending fails when there's not enough
                    // channel space, but we called poll_ready
                    panic!("called call without poll_ready");
                }
            }
            Ok(()) => {
                // The receiver end of the oneshot is itself a future.
                rx.map(|oneshot_recv_result| {
                    // The ClientRequest oneshot sender should not be dropped before sending a
                    // response. But sometimes that happens during process or connection shutdown.
                    // So we just return a generic error here.
                    match oneshot_recv_result {
                        Ok(result) => result,
                        Err(oneshot::Canceled) => Err(PeerError::ConnectionDropped.into()),
                    }
                })
                .boxed()
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Make sure there is an error in the slot
        let drop_error: SharedPeerError = PeerError::ClientDropped.into();
        let original_error = self.error_slot.try_update_error(drop_error.clone());
        debug!(
            ?original_error,
            latest_error = ?drop_error,
            "client struct dropped"
        );

        self.shutdown();
    }
}
