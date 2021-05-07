use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::prelude::*;
use tokio::net::TcpStream;
use tower::{discover::Change, Service, ServiceExt};

use crate::{BoxError, Request, Response};

use super::{Client, ConnectedAddr, Handshake};

/// A wrapper around [`peer::Handshake`] that opens a TCP connection before
/// forwarding to the inner handshake service. Writing this as its own
/// [`tower::Service`] lets us apply unified timeout policies, etc.
pub struct Connector<S> {
    handshaker: Handshake<S>,
}

impl<S: Clone> Clone for Connector<S> {
    fn clone(&self) -> Self {
        Connector {
            handshaker: self.handshaker.clone(),
        }
    }
}

impl<S> Connector<S> {
    pub fn new(handshaker: Handshake<S>) -> Self {
        Connector { handshaker }
    }
}

impl<S> Service<SocketAddr> for Connector<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = Change<SocketAddr, Client>;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, addr: SocketAddr) -> Self::Future {
        let mut hs = self.handshaker.clone();
        async move {
            let stream = TcpStream::connect(addr).await?;
            hs.ready_and().await?;
            let connected_addr = ConnectedAddr::new_outbound_direct(addr);
            let client = hs.call((stream, connected_addr)).await?;
            Ok(Change::Insert(addr, client))
        }
        .boxed()
    }
}
