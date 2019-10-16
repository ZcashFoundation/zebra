use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    channel::{mpsc, oneshot},
    future, ready,
};
use tokio::prelude::*;
use tower::Service;

use crate::protocol::internal::{Request, Response};

use super::{error::ErrorSlot, SharedPeerError};

/// The "client" duplex half of a peer connection.
pub struct PeerClient {
    pub(super) span: tracing::Span,
    pub(super) server_tx: mpsc::Sender<ClientRequest>,
    pub(super) error_slot: ErrorSlot,
}

/// A message from the `PeerClient` to the `PeerServer`, containing both a
/// request and a return message channel. The reason the return channel is
/// included is because `PeerClient::call` returns a future that may be moved
/// around before it resolves, so the future must have ownership of the channel
/// on which it receives the response.
#[derive(Debug)]
pub(super) struct ClientRequest(
    pub(super) Request,
    pub(super) oneshot::Sender<Result<Response, SharedPeerError>>,
);

impl Service<Request> for PeerClient {
    type Response = Response;
    type Error = SharedPeerError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Err(_) = ready!(self.server_tx.poll_ready(cx)) {
            Poll::Ready(Err(self
                .error_slot
                .try_get_error()
                .expect("failed PeerServers must set their error slot")))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        use futures::future::FutureExt;
        use tracing_futures::Instrument;

        let (tx, rx) = oneshot::channel();
        match self.server_tx.try_send(ClientRequest(req, tx)) {
            Err(e) => {
                if e.is_disconnected() {
                    future::ready(Err(self
                        .error_slot
                        .try_get_error()
                        .expect("failed PeerServers must set their error slot")))
                    .instrument(self.span.clone())
                    .boxed()
                } else {
                    // sending fails when there's not enough
                    // channel space, but we called poll_ready
                    panic!("called call without poll_ready");
                }
            }
            Ok(()) => {
                // The reciever end of the oneshot is itself a future.
                rx.map(|oneshot_recv_result| {
                    oneshot_recv_result
                        .expect("ClientRequest oneshot sender must not be dropped before send")
                })
                .instrument(self.span.clone())
                .boxed()
            }
        }
    }
}
