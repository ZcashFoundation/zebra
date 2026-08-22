//! Wrapper around version 2 (QUIC) peer connection handshakes.
//!
//! This is the version 2 analog of the legacy
//! [`Connector`](crate::peer::Connector): both answer the same
//! [`OutboundConnectorRequest`] with the same [`Client`], so the crawler
//! dials either transport through one interface.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::FutureExt;
use tower::{Service, ServiceExt};
use tracing::{info_span, Instrument};

use zebra_chain::{chain_tip::ChainTip, parameters::Network};

use crate::{
    peer::{
        v2::{V2HandshakeRequest, V2Handshaker},
        Client, ConnectedAddr, OutboundConnectorRequest,
    },
    protocol::v2::quic,
    BoxError, PeerSocketAddr, Request, Response,
};

/// A wrapper around [`peer::v2::Handshake`](crate::peer::v2::Handshake) that
/// opens a QUIC connection before the handshake.
#[derive(Clone)]
pub struct Connector<S, C = zebra_chain::chain_tip::NoChainTip> {
    /// The QUIC endpoint outbound connections are opened from.
    ///
    /// This is the endpoint that also accepts inbound connections, so both
    /// directions share one UDP socket.
    endpoint: quinn::Endpoint,

    /// The configured network, which selects the ALPN identifier.
    network: Network,

    /// The version 2 handshake service.
    handshaker: V2Handshaker<S, C>,
}

impl<S, C> Connector<S, C> {
    /// Creates a version 2 connector over `endpoint`.
    pub fn new(
        endpoint: quinn::Endpoint,
        network: Network,
        handshaker: V2Handshaker<S, C>,
    ) -> Self {
        Connector {
            endpoint,
            network,
            handshaker,
        }
    }
}

impl<S, C> Service<OutboundConnectorRequest> for Connector<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
    C: ChainTip + Clone + Send + Sync + 'static,
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
        } = req;

        let endpoint = self.endpoint.clone();
        let network = self.network.clone();
        let handshaker = self.handshaker.clone();
        let connected_addr = ConnectedAddr::new_outbound_direct(addr);
        let connector_span = info_span!("v2_connector", peer = ?connected_addr);

        // # Security
        //
        // The caller times this future out. Dropping it cancels the dial and
        // releases the connection tracker, so an abandoned attempt cannot
        // hold an outbound slot until the transport's idle timeout.
        async move {
            let connection = quic::connect(&endpoint, *addr, &network).await?;
            let client = handshaker
                .oneshot(V2HandshakeRequest {
                    connection,
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
