use std::{
    pin::Pin,
    task::{Context, Poll},
};

use failure::Error;
use futures::{
    channel::{mpsc, oneshot},
    future, ready,
};
use tokio::prelude::*;
use tower::Service;

use crate::protocol::internal::{Request, Response};

use super::{server::ErrorSlot, PeerError};

/// The "client" duplex half of a peer connection.
pub struct PeerClient {
    pub(super) server_tx: mpsc::Sender<ClientRequest>,
    pub(super) error_slot: ErrorSlot,
}

/// A message from the `PeerClient` to the `PeerServer`, containing both a
/// request and a return message channel. The reason the return channel is
/// included is because `PeerClient::call` returns a future that may be moved
/// around before it resolves, so the future must have ownership of the channel
/// on which it receives the response.
pub(super) struct ClientRequest(
    pub(super) Request,
    pub(super) oneshot::Sender<Result<Response, PeerError>>,
);

impl Service<Request> for PeerClient {
    type Response = Response;
    type Error = PeerError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

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
        use futures::future::{FutureExt, TryFutureExt};
        let (tx, rx) = oneshot::channel();
        match self.server_tx.try_send(ClientRequest(req, tx)) {
            Err(e) => {
                if e.is_disconnected() {
                    future::ready(Err(self
                        .error_slot
                        .try_get_error()
                        .expect("failed PeerServers must set their error slot")))
                    .boxed()
                } else {
                    // sending fails when there's not enough
                    // channel space, but we called poll_ready
                    panic!("called call without poll_ready");
                }
            }
            // need a bit of yoga to get result types to align,
            // because the oneshot future can error
            Ok(()) => rx
                .map(|val| match val {
                    Ok(Ok(rsp)) => Ok(rsp),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(format_err!("oneshot died").into()),
                })
                .boxed(),
        }
    }
}
