use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use failure::Error;
use tokio::prelude::*;
use tower::Service;

use super::{client::PeerClient, server::PeerServer};

/// A [`Service`] that connects to a remote peer and constructs a client/server pair.
pub struct PeerConnector {
    // Eventually this will need to have state, but for now just take an address to conncet to.
}

impl Service<SocketAddr> for PeerConnector {
    type Response = (PeerClient, PeerServer);
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // XXX when this asks a second service for
        // an address to connect to, it should call inner.ready
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, addr: SocketAddr) -> Self::Future {
        let fut = async move {
            warn!(addr = ?addr);

            Ok((PeerClient {}, PeerServer {}))
        };

        fut.boxed()
    }
}
