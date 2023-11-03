//! Implements methods for testing [`Handshake`]

use super::*;

impl<S, C> Handshake<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    /// Returns a count of how many connection nonces are stored in this [`Handshake`]
    #[allow(dead_code)]
    pub async fn nonce_count(&self) -> usize {
        self.nonces.lock().await.len()
    }
}

impl Default for ConnectedAddr {
    fn default() -> Self {
        // This is a documentation and examples IP address:
        // <https://en.wikipedia.org/wiki/Reserved_IP_addresses>
        Self::OutboundDirect {
            addr: "198.51.100.0:8233"
                .parse()
                .expect("hard-coded address is valid"),
        }
    }
}
