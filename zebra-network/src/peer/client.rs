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

use super::{ErrorSlot, SharedPeerError};

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
    pub tx: oneshot::Sender<Result<Response, SharedPeerError>>,
    /// The tracing context for the request, so that work the connection task does
    /// processing messages in the context of this request will have correct context.
    pub span: tracing::Span,
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

    fn call(&mut self, request: Request) -> Self::Future {
        use futures::future::FutureExt;

        let (tx, rx) = oneshot::channel();
        // get the current Span to propagate it to the peer connection task.
        // this allows the peer connection to enter the correct tracing context
        // when it's handling messages in the context of processing this
        // request.
        let span = tracing::Span::current();

        match self.server_tx.try_send(ClientRequest { request, span, tx }) {
            Err(e) => {
                if e.is_disconnected() {
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
