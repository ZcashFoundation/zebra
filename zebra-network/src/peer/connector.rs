//! Wrapper around handshake logic that also opens a TCP connection.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::prelude::*;
use tokio::net::TcpStream;
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::chain_tip::{ChainTip, NoChainTip};

use crate::{
    peer::{Client, ConnectedAddr, Handshake, HandshakeRequest},
    peer_set::ConnectionTracker,
    BoxError, PeerSocketAddr, Request, Response,
};

/// A wrapper around [`Handshake`] that opens a TCP connection before
/// forwarding to the inner handshake service. Writing this as its own
/// [`tower::Service`] lets us apply unified timeout policies, etc.
pub struct Connector<S, C = NoChainTip>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    handshaker: Handshake<S, C>,
}

impl<S, C> Clone for Connector<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    fn clone(&self) -> Self {
        Connector {
            handshaker: self.handshaker.clone(),
        }
    }
}

impl<S, C> Connector<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    pub fn new(handshaker: Handshake<S, C>) -> Self {
        Connector { handshaker }
    }
}

/// A connector request.
/// Contains the information needed to make an outbound connection to the peer.
pub struct OutboundConnectorRequest {
    /// The Zcash listener address of the peer.
    pub addr: PeerSocketAddr,

    /// A connection tracker that reduces the open connection count when dropped.
    ///
    /// Used to limit the number of open connections in Zebra.
    pub connection_tracker: ConnectionTracker,
}

impl<S, C> Service<OutboundConnectorRequest> for Connector<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    type Response = (PeerSocketAddr, Client);
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: OutboundConnectorRequest) -> Self::Future {
        let OutboundConnectorRequest {
            addr,
            connection_tracker,
        }: OutboundConnectorRequest = req;

        let hs = self.handshaker.clone();
        let connected_addr = ConnectedAddr::new_outbound_direct(addr);
        let connector_span = info_span!("connector", peer = ?connected_addr);

        // # Security
        //
        // `zebra_network::init()` implements a connection timeout on this future.
        // Any code outside this future does not have a timeout.
        async move {
            let tcp_stream = TcpStream::connect(*addr).await?;
            let client = hs
                .oneshot(HandshakeRequest::<TcpStream> {
                    data_stream: tcp_stream,
                    connected_addr,
                    connection_tracker,
                })
                .await?;
            Ok((addr, client))
        }
        .instrument(connector_span)
        .boxed()
    }
}
