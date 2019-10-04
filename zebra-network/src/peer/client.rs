use std::{
    pin::Pin,
    task::{Context, Poll},
};

use failure::Error;
use futures::{channel::mpsc, future, ready};
use tokio::prelude::*;
use tower::Service;

use crate::protocol::internal::{Request, Response};

use super::{server::ErrorSlot, PeerError};

/// The "client" duplex half of a peer connection.
pub struct PeerClient {
    pub(super) server_rx: mpsc::Receiver<Result<Response, PeerError>>,
    pub(super) server_tx: mpsc::Sender<Request>,
    pub(super) error_slot: ErrorSlot,
}

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
        unimplemented!();
        /*
        match self.server_tx.try_send(req) {
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
            // This doesn't work, because the returned future's lifetime
            // needs to be independent of the lifetime of `self`, but
            // because the returned future references the `rx` channel,
            // the channel must outlive the returned future.
            Ok(()) => self
                .server_rx
                .next()
                .map(|opt| opt.unwrap_or_else(|| Err(format_err!("server disconnected"))))
                .boxed(),
        }
        */
    }
}
