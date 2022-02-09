//! Handles outbound requests from our node to the network.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    channel::{mpsc, oneshot},
    future, ready,
    stream::{Stream, StreamExt},
    FutureExt,
};
use tokio::task::JoinHandle;
use tower::Service;

use crate::{
    peer::error::AlreadyErrored,
    protocol::{
        external::types::Version,
        internal::{Request, Response},
    },
};

use super::{ErrorSlot, PeerError, SharedPeerError};

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

/// The "client" duplex half of a peer connection.
pub struct Client {
    /// Used to shut down the corresponding heartbeat.
    /// This is always Some except when we take it on drop.
    pub(crate) shutdown_tx: Option<oneshot::Sender<CancelHeartbeatTask>>,

    /// Used to send [`Request`]s to the remote peer.
    pub(crate) server_tx: mpsc::Sender<ClientRequest>,

    /// A slot for an error shared between the Connection and the Client that uses it.
    ///
    /// `None` unless the connection or client have errored.
    pub(crate) error_slot: ErrorSlot,

    /// The peer connection's protocol version.
    pub(crate) version: Version,

    /// A handle to the task responsible for connecting to the peer.
    pub(crate) connection_task: JoinHandle<()>,

    /// A handle to the task responsible for sending periodic heartbeats.
    pub(crate) heartbeat_task: JoinHandle<()>,
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

    /// The response [`Message`] channel, included because `peer::Client::call` returns a
    /// future that may be moved around before it resolves.
    pub tx: oneshot::Sender<Result<Response, SharedPeerError>>,

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
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // skip the channels, they don't tell us anything useful
        f.debug_struct("Client")
            .field("error_slot", &self.error_slot)
            .finish()
    }
}

impl From<ClientRequest> for InProgressClientRequest {
    fn from(client_request: ClientRequest) -> Self {
        let ClientRequest { request, tx, span } = client_request;
        InProgressClientRequest {
            request,
            tx: tx.into(),
            span,
        }
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
        self.inner
            .try_next()
            .expect("channel is closed")
            .map(Into::into)
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
    /// Forwards `response` to `tx.send()`, and marks this sender as used.
    ///
    /// Panics if `tx.send()` is used more than once.
    pub fn send(
        mut self,
        response: Result<Response, SharedPeerError>,
    ) -> Result<(), Result<Response, SharedPeerError>> {
        self.tx
            .take()
            .unwrap_or_else(|| {
                panic!(
                    "multiple uses of oneshot sender: oneshot must be used exactly once: {:?}",
                    self
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
                || panic!("called is_canceled() after using oneshot sender: oneshot must be used exactly once: {:?}", self))
    }
}

impl From<oneshot::Sender<Result<Response, SharedPeerError>>> for MustUseClientResponseSender {
    fn from(sender: oneshot::Sender<Result<Response, SharedPeerError>>) -> Self {
        MustUseClientResponseSender { tx: Some(sender) }
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
                "unused client response sender: oneshot must be used or canceled: {:?}",
                self
            );
        }
    }
}

impl Client {
    /// Check if this connection's heartbeat task has exited.
    fn check_heartbeat(&mut self, cx: &mut Context<'_>) -> Result<(), SharedPeerError> {
        let is_canceled = self
            .shutdown_tx
            .as_mut()
            .expect("only taken on drop")
            .poll_canceled(cx)
            .is_ready();

        if is_canceled {
            return self.set_task_exited_error("heartbeat", PeerError::HeartbeatTaskExited);
        }

        match self.heartbeat_task.poll_unpin(cx) {
            Poll::Pending => {
                // Heartbeat task is still running.
                Ok(())
            }
            Poll::Ready(Ok(())) => {
                // Heartbeat task stopped unexpectedly, without panicking.
                self.set_task_exited_error("heartbeat", PeerError::HeartbeatTaskExited)
            }
            Poll::Ready(Err(error)) => {
                // Heartbeat task stopped unexpectedly with a panic.
                panic!("heartbeat task has panicked: {}", error);
            }
        }
    }

    /// Check if the connection's task has exited.
    fn check_connection(&mut self, context: &mut Context<'_>) -> Result<(), SharedPeerError> {
        match self.connection_task.poll_unpin(context) {
            Poll::Pending => {
                // Connection task is still running.
                Ok(())
            }
            Poll::Ready(Ok(())) => {
                // Connection task stopped unexpectedly, without panicking.
                self.set_task_exited_error("connection", PeerError::ConnectionTaskExited)
            }
            Poll::Ready(Err(error)) => {
                // Connection task stopped unexpectedly with a panic.
                panic!("connection task has panicked: {}", error);
            }
        }
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
    fn poll_request(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SharedPeerError>> {
        if ready!(self.server_tx.poll_ready(cx)).is_err() {
            Poll::Ready(Err(self
                .error_slot
                .try_get_error()
                .expect("failed servers must set their error slot")))
        } else if let Some(error) = self.error_slot.try_get_error() {
            Poll::Ready(Err(error))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    /// Shut down the resources held by the client half of this peer connection.
    ///
    /// Stops further requests to the remote peer, and stops the heartbeat task.
    fn shutdown(&mut self) {
        // Prevent any senders from sending more messages to this peer.
        self.server_tx.close_channel();

        // Stop the heartbeat task
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(CancelHeartbeatTask);
        }
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
        // `check_heartbeat` and `check_connection` schedule the client task for wakeup
        // if either task exits, or if the heartbeat task drops the cancel handle.
        //
        //`ready!` returns `Poll::Pending` when `server_tx` is unready, and
        // schedules this task for wakeup.

        let mut result = self
            .check_heartbeat(cx)
            .and_then(|()| self.check_connection(cx));

        if result.is_ok() {
            result = ready!(self.poll_request(cx));
        }

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

        match self.server_tx.try_send(ClientRequest { request, tx, span }) {
            Err(e) => {
                if e.is_disconnected() {
                    let ClientRequest { tx, .. } = e.into_inner();
                    let _ = tx.send(Err(PeerError::ConnectionClosed.into()));
                    future::ready(Err(self
                        .error_slot
                        .try_get_error()
                        .expect("failed servers must set their error slot")))
                    .boxed()
                } else {
                    // sending fails when there's not enough
                    // channel space, but we called poll_ready
                    panic!("called call without poll_ready");
                }
            }
            Ok(()) => {
                // The receiver end of the oneshot is itself a future.
                rx.map(|oneshot_recv_result| {
                    oneshot_recv_result
                        .expect("ClientRequest oneshot sender must not be dropped before send")
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
