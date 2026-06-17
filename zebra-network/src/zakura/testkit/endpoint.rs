//! Relay-free loopback Iroh endpoints for Zakura tests.

use std::net::{Ipv4Addr, SocketAddrV4};

use blake2b_simd::Params as Blake2bParams;
use iroh::{endpoint::TransportConfig, Endpoint, NodeAddr, SecretKey, Watcher as _};

use crate::{zakura::direct_endpoint_builder, BoxError};

/// Factory for deterministic relay-free loopback Iroh endpoints.
#[derive(Debug)]
pub struct LocalEndpointFactory {
    transport_config: Option<TransportConfig>,
}

impl LocalEndpointFactory {
    /// Create a factory with Iroh's default transport configuration.
    pub fn new() -> Self {
        Self {
            transport_config: None,
        }
    }

    /// Create a factory with a caller-supplied transport configuration.
    pub fn with_transport_config(transport_config: TransportConfig) -> Self {
        Self {
            transport_config: Some(transport_config),
        }
    }

    /// Deterministically derive an Iroh secret key from a small seed.
    pub fn secret_key(seed: u64) -> SecretKey {
        let mut seed_bytes = [0; 8];
        seed_bytes.copy_from_slice(&seed.to_le_bytes());
        let digest = Blake2bParams::new()
            .hash_length(32)
            .personal(b"zakura-test-key")
            .to_state()
            .update(&seed_bytes)
            .finalize();
        let mut key_bytes = [0; 32];
        key_bytes.copy_from_slice(digest.as_bytes());
        SecretKey::from_bytes(&key_bytes)
    }

    /// Bind a relay-free endpoint to an OS-assigned loopback port.
    pub async fn endpoint(self, seed: u64) -> Result<Endpoint, BoxError> {
        let mut builder = direct_endpoint_builder(Self::secret_key(seed))
            .bind_addr_v4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        if let Some(transport_config) = self.transport_config {
            builder = builder.transport_config(transport_config);
        }
        let endpoint = builder.bind().await?;

        Ok(endpoint)
    }

    /// Return a fully initialized direct node address.
    pub async fn node_addr(endpoint: &Endpoint) -> NodeAddr {
        endpoint.node_addr().initialized().await
    }

    /// Teach `a` how to reach `b` directly.
    pub async fn wire(a: &Endpoint, b: &Endpoint) -> Result<(), BoxError> {
        a.add_node_addr(Self::node_addr(b).await)?;
        Ok(())
    }
}

impl Default for LocalEndpointFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler, Router},
    };

    #[derive(Debug, Clone)]
    struct Noop;

    impl ProtocolHandler for Noop {
        async fn accept(&self, _connection: Connection) -> Result<(), AcceptError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn endpoint_factory_is_deterministic_and_relay_free() -> Result<(), BoxError> {
        let key_a = LocalEndpointFactory::secret_key(1);
        let key_b = LocalEndpointFactory::secret_key(1);
        assert_eq!(key_a.public(), key_b.public());

        let endpoint = LocalEndpointFactory::new().endpoint(1).await?;
        let node_addr = LocalEndpointFactory::node_addr(&endpoint).await;

        assert_eq!(node_addr.node_id, endpoint.node_id());
        assert!(node_addr.relay_url().is_none());
        assert!(node_addr.direct_addresses().next().is_some());
        assert!(endpoint.discovery().is_none());

        endpoint.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn factory_endpoints_connect_over_loopback() -> Result<(), BoxError> {
        const ALPN: &[u8] = b"/zakura/testkit/noop/0";

        let server = LocalEndpointFactory::new().endpoint(10).await?;
        let router = Router::builder(server).accept(ALPN, Noop).spawn();
        let client = LocalEndpointFactory::new().endpoint(11).await?;
        let server_addr = router.endpoint().node_addr().initialized().await;
        client.add_node_addr(server_addr.clone())?;

        let connection = client.connect(server_addr, ALPN).await?;
        assert_eq!(connection.remote_node_id()?, router.endpoint().node_id());

        connection.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await?;
        Ok(())
    }
}
