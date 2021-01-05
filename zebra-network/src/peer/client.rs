use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    channel::{mpsc, oneshot},
    future, ready,
};
use tower::Service;

use crate::protocol::internal::{Request, Response};

use super::{ErrorSlot, PeerError, SharedPeerError};

/// The "client" duplex half of a peer connection.
pub struct Client {
    // Used to shut down the corresponding heartbeat.
    // This is always Some except when we take it on drop.
    pub(super) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(super) server_tx: mpsc::Sender<ClientRequest>,
    pub(super) error_slot: ErrorSlot,
}

/// A message from the `peer::Client` to the `peer::Server`.
#[derive(Debug)]
#[must_use = "tx.send() must be called before drop"]
pub(super) struct ClientRequest {
    /// The actual request.
    pub request: Request,
    /// The return message channel, included because `peer::Client::call` returns a
    /// future that may be moved around before it resolves.
    ///
    /// INVARIANT: `tx.send()` must be called before dropping `tx`.
    ///
    /// JUSTIFICATION: the `peer::Client` will translate all `Request`s into a `ClientRequest` which it sends to a background task, and if the send replies with `Ok(())` it will assume that it is safe to unconditionally poll the `Receiver` tied to the `Sender` used to create the `ClientRequest`.
    pub tx: MustUseOneshotSender<Result<Response, SharedPeerError>>,
    /// The tracing context for the request, so that work the connection task does
    /// processing messages in the context of this request will have correct context.
    pub span: tracing::Span,
}

/// A oneshot::Sender that must be used by calling `send()`.
///
/// Panics on drop if `tx` has not been used or canceled.
/// Panics if `tx.send()` is used more than once.
#[derive(Debug)]
#[must_use = "tx.send() must be called before drop"]
pub(super) struct MustUseOneshotSender<T: std::fmt::Debug> {
    /// The sender for the oneshot channel.
    ///
    /// `None` if `tx.send()` has been used.
    pub tx: Option<oneshot::Sender<T>>,
}

impl<T: std::fmt::Debug> MustUseOneshotSender<T> {
    /// Forwards `t` to `tx.send()`, and marks this sender as used.
    ///
    /// Panics if `tx.send()` is used more than once.
    #[instrument(skip(self))]
    pub fn send(mut self, t: T) -> Result<(), T> {
        self.tx
            .take()
            .unwrap_or_else(|| {
                panic!(
                    "multiple uses of oneshot sender: oneshot must be used exactly once: {:?}",
                    self
                )
            })
            .send(t)
    }

    /// Returns `tx.cancellation()`.
    ///
    /// Panics if `tx.send()` has previously been used.
    #[instrument(skip(self))]
    pub fn cancellation(&mut self) -> oneshot::Cancellation<'_, T> {
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
    #[instrument(skip(self))]
    pub fn is_canceled(&self) -> bool {
        self.tx
            .as_ref()
            .map(|tx| tx.is_canceled())
            .unwrap_or_else(
                || panic!("called is_canceled() after using oneshot sender: oneshot must be used exactly once: {:?}", self))
    }
}

impl<T: std::fmt::Debug> From<oneshot::Sender<T>> for MustUseOneshotSender<T> {
    fn from(sender: oneshot::Sender<T>) -> Self {
        MustUseOneshotSender { tx: Some(sender) }
    }
}

impl<T: std::fmt::Debug> Drop for MustUseOneshotSender<T> {
    #[instrument(skip(self))]
    fn drop(&mut self) {
        // is_canceled() will not panic, because we check is_none() first
        assert!(
            self.tx.is_none() || self.is_canceled(),
            "unused oneshot sender: oneshot must be used or canceled: {:?}",
            self
        );
    }
}

impl Service<Request> for Client {
    type Response = Response;
    type Error = SharedPeerError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if ready!(self.server_tx.poll_ready(cx)).is_err() {
            Poll::Ready(Err(self
                .error_slot
                .try_get_error()
                .expect("failed servers must set their error slot")))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    #[instrument(skip(self))]
    fn call(&mut self, request: Request) -> Self::Future {
        use futures::future::FutureExt;

        let (tx, rx) = oneshot::channel();
        // get the current Span to propagate it to the peer connection task.
        // this allows the peer connection to enter the correct tracing context
        // when it's handling messages in the context of processing this
        // request.
        let span = tracing::Span::current();

        match self.server_tx.try_send(ClientRequest {
            request,
            span,
            tx: tx.into(),
        }) {
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
        let _ = self
            .shutdown_tx
            .take()
            .expect("must not drop twice")
            .send(());
    }
}
