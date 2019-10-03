use std::{
    pin::Pin,
    task::{Context, Poll},
};

use failure::Error;
use futures::channel::mpsc;
use tokio::prelude::*;
use tower::Service;

use crate::protocol::internal::{Request, Response};

use super::server::Handle;

/// The "client" duplex half of a peer connection.
pub struct PeerClient {
    pub(super) server_rx: mpsc::Receiver<Result<Response, Error>>,
    pub(super) server_tx: mpsc::Sender<Request>,
    pub(super) handle: Handle,
}

impl Service<Request> for PeerClient {
    type Response = Response;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        unimplemented!();
    }

    fn call(&mut self, req: Request) -> Self::Future {
        unimplemented!();
    }
}
