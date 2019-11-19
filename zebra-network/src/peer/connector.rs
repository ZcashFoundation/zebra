use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{net::TcpStream, prelude::*};
use tower::{discover::Change, Service, ServiceExt};

use crate::{BoxedStdError, Request, Response};

use super::{HandshakeError, PeerClient, PeerHandshake};

/// A wrapper around [`PeerHandshake`] that opens a TCP connection before
/// forwarding to the inner handshake service. Writing this as its own
/// [`tower::Service`] lets us apply unified timeout policies, etc.
pub struct PeerConnector<S> {
    handshaker: PeerHandshake<S>,
}

impl<S: Clone> Clone for PeerConnector<S> {
    fn clone(&self) -> Self {
        Self {
            handshaker: self.handshaker.clone(),
        }
    }
}

impl<S> PeerConnector<S> {
    pub fn new(handshaker: PeerHandshake<S>) -> Self {
        Self { handshaker }
    }
}

impl<S> Service<SocketAddr> for PeerConnector<S>
where
    S: Service<Request, Response = Response, Error = BoxedStdError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = Change<SocketAddr, PeerClient>;
    type Error = HandshakeError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, addr: SocketAddr) -> Self::Future {
        let mut hs = self.handshaker.clone();
        async move {
            let stream = TcpStream::connect(addr).await?;
            hs.ready().await?;
            let client = hs.call((stream, addr)).await?;
            Ok(Change::Insert(addr, client))
        }
            .boxed()
    }
}
