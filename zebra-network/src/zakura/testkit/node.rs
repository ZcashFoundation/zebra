//! In-process Zakura test node built from the production handler.

use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use iroh::{endpoint::TransportConfig, protocol::Router, NodeAddr, NodeId};
use tokio::{sync::Mutex, task::JoinHandle};
use zebra_jsonl_trace::JsonlTracer;

use super::{InboundRecorder, LocalEndpointFactory, WaitError};
use crate::{
    zakura::{
        discovery::build_discovery_handle, service_registry, DiscoveryService, Service,
        ZakuraDiscoveryHandle, ZakuraEndpoint, ZakuraHandshakeConfig, ZakuraLocalLimits,
        ZakuraPeerId, ZakuraProtocolHandler, ZakuraServiceId, ZakuraSupervisorHandle, ZakuraTrace,
        P2P_V2_ALPN,
    },
    BoxError, Config,
};

/// A running in-process Zakura node for integration tests.
#[derive(Debug)]
pub struct ZakuraTestNode {
    seed: u64,
    endpoint: ZakuraEndpoint,
    discovery: ZakuraDiscoveryHandle,
    limits: ZakuraLocalLimits,
    recorder: InboundRecorder,
    dial_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    _tracer: JsonlTracer,
}

impl ZakuraTestNode {
    /// Create a node builder using `seed` as the deterministic identity.
    pub fn builder(seed: u64) -> ZakuraTestNodeBuilder {
        ZakuraTestNodeBuilder::new(seed)
    }

    /// Deterministic seed used by this node.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Current Iroh node address.
    pub async fn node_addr(&self) -> NodeAddr {
        self.endpoint.node_addr().await
    }

    /// Active supervisor handle.
    pub fn supervisor(&self) -> ZakuraSupervisorHandle {
        self.endpoint.supervisor()
    }

    /// Local limits used by this node.
    pub fn limits(&self) -> &ZakuraLocalLimits {
        &self.limits
    }

    /// Bounded inbound recorder.
    pub fn recorder(&self) -> InboundRecorder {
        self.recorder.clone()
    }

    /// Native discovery runtime handle backing this node's discovery service.
    pub fn discovery(&self) -> ZakuraDiscoveryHandle {
        self.discovery.clone()
    }

    /// Spawn this node's discovery candidate dialer (book-driven outbound dials).
    pub fn spawn_discovery_dialer(&self) -> JoinHandle<()> {
        tokio::spawn(crate::zakura::discovery::run_native_discovery_dialer(
            self.endpoint.clone(),
            self.discovery.clone(),
            self.limits.clone(),
        ))
    }

    /// Insert `peer` as a trusted static discovery candidate (loopback allowed)
    /// and teach iroh its route, so the candidate dialer can connect to it.
    pub async fn insert_static_discovery_candidate(
        &self,
        peer: &ZakuraTestNode,
    ) -> Result<NodeId, BoxError> {
        let node_addr = peer.node_addr().await;
        let node_id = node_addr.node_id;
        self.endpoint.add_node_addr(node_addr.clone())?;
        self.discovery.insert_static_candidate(node_addr).await?;
        Ok(node_id)
    }

    /// Start a native dial to `peer` and wait until this node registers it.
    pub async fn connect_native(
        &self,
        peer: &ZakuraTestNode,
        timeout: Duration,
    ) -> Result<(), BoxError> {
        let peer_addr = peer.node_addr().await;
        self.endpoint.add_node_addr(peer_addr.clone())?;
        let mut handle = self.endpoint.spawn_native_dial(peer_addr.clone());
        let peer_id = peer_addr.node_id.as_bytes().to_vec();
        let mut peer_set_rx = self.supervisor().subscribe();

        let result = tokio::time::timeout(timeout, async {
            tokio::select! {
                registered = wait_for_peer_registration(&mut peer_set_rx, peer_id.as_slice()) => {
                    registered
                }
                joined = &mut handle => {
                    joined
                        .map_err(|error| -> BoxError { format!("native Zakura dial task failed: {error}").into() })?;
                    Err("native Zakura dial task ended before serving the connection".into())
                }
            }
        })
        .await;

        match result {
            Ok(Ok(())) => {
                self.dial_tasks.lock().await.push(handle);
                Ok(())
            }
            Ok(Err(error)) => {
                handle.abort();
                Err(error)
            }
            Err(_) => {
                handle.abort();
                Err(Box::new(WaitError::new(
                    "native Zakura peer registration",
                    timeout,
                )))
            }
        }
    }

    /// Shut the node down and abort outstanding dial tasks.
    pub async fn shutdown(&self) {
        self.endpoint.shutdown().await;
        let mut tasks = self.dial_tasks.lock().await;
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

async fn wait_for_peer_registration(
    peer_set_rx: &mut tokio::sync::watch::Receiver<Vec<ZakuraPeerId>>,
    peer_id: &[u8],
) -> Result<(), BoxError> {
    loop {
        if peer_set_rx
            .borrow()
            .iter()
            .any(|id| id.as_bytes() == peer_id)
        {
            return Ok(());
        }

        peer_set_rx.changed().await.map_err(|_| -> BoxError {
            "Zakura peer-set watcher closed before registration".into()
        })?;
    }
}

/// Builder for [`ZakuraTestNode`].
pub struct ZakuraTestNodeBuilder {
    seed: u64,
    limits: ZakuraLocalLimits,
    transport_config: Option<TransportConfig>,
    legacy_upgrade: bool,
    tracer: JsonlTracer,
    service: Option<Arc<dyn Service>>,
    service_factory: Option<Box<dyn FnOnce(ZakuraSupervisorHandle) -> Arc<dyn Service> + Send>>,
    discovery_direct_addrs: Vec<SocketAddr>,
    extra_advertised_services: Vec<ZakuraServiceId>,
}

impl fmt::Debug for ZakuraTestNodeBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZakuraTestNodeBuilder")
            .field("seed", &self.seed)
            .field("limits", &self.limits)
            .field("transport_config", &self.transport_config.is_some())
            .field("legacy_upgrade", &self.legacy_upgrade)
            .field("tracer", &self.tracer)
            .field(
                "service",
                &(self.service.is_some() || self.service_factory.is_some()),
            )
            .finish()
    }
}

impl ZakuraTestNodeBuilder {
    /// Create a node builder.
    pub fn new(seed: u64) -> Self {
        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = 16;
        limits.max_pending_handshakes = 8;
        limits.max_open_streams = 16;
        limits.max_inbound_queue_depth = 64;
        Self {
            seed,
            limits,
            transport_config: None,
            legacy_upgrade: false,
            tracer: JsonlTracer::noop(),
            service: None,
            service_factory: None,
            discovery_direct_addrs: Vec::new(),
            extra_advertised_services: Vec::new(),
        }
    }

    /// Advertise these direct addresses in this node's discovery self-record.
    pub fn discovery_direct_addrs(mut self, direct_addrs: Vec<SocketAddr>) -> Self {
        self.discovery_direct_addrs = direct_addrs;
        self
    }

    /// Advertise an additional service id in this node's discovery self-record.
    pub fn add_advertised_service(mut self, service: ZakuraServiceId) -> Self {
        self.extra_advertised_services.push(service);
        self
    }

    /// Override local limits.
    pub fn limits(mut self, limits: ZakuraLocalLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Mutate the transport configuration used by the endpoint factory.
    pub fn transport(mut self, configure: impl FnOnce(&mut TransportConfig)) -> Self {
        let mut transport = self.limits.transport_config();
        configure(&mut transport);
        self.transport_config = Some(transport);
        self
    }

    /// Reserve the legacy-upgrade test hook. Native-only remains the default.
    pub fn enable_legacy_upgrade(mut self, enable: bool) -> Self {
        self.legacy_upgrade = enable;
        self
    }

    /// Reserve the JSONL tracer hook used by the trace-introspection plan.
    pub fn tracer(mut self, tracer: JsonlTracer) -> Self {
        self.tracer = tracer;
        self
    }

    /// Install a custom service instead of the default recorder.
    pub fn service(mut self, service: Arc<dyn Service>) -> Self {
        self.service = Some(service);
        self
    }

    /// Install a custom service that needs this node's supervisor.
    pub fn service_from_supervisor(
        mut self,
        factory: impl FnOnce(ZakuraSupervisorHandle) -> Arc<dyn Service> + Send + 'static,
    ) -> Self {
        self.service_factory = Some(Box::new(factory));
        self
    }

    /// Spawn the node.
    pub async fn spawn(self) -> Result<ZakuraTestNode, BoxError> {
        if self.legacy_upgrade {
            return Err(
                "ZakuraTestNode legacy-upgrade mode is reserved until connect_via_upgrade is implemented"
                    .into(),
            );
        }

        let transport = self
            .transport_config
            .unwrap_or_else(|| self.limits.transport_config());
        let endpoint = LocalEndpointFactory::with_transport_config(transport)
            .endpoint(self.seed)
            .await?;
        let supervisor = ZakuraSupervisorHandle::new(self.limits.max_connections);
        let recorder = InboundRecorder::new(usize::from(self.limits.max_inbound_queue_depth));
        let base_service = if let Some(factory) = self.service_factory {
            factory(supervisor.clone())
        } else {
            self.service.unwrap_or_else(|| Arc::new(recorder.clone()))
        };
        let network = Config::default().network;
        let handshake_config = ZakuraHandshakeConfig::for_network(&network);
        let mut advertised_services = crate::zakura::discovery::default_advertised_services();
        advertised_services.extend(self.extra_advertised_services.clone());
        let discovery = build_discovery_handle(
            LocalEndpointFactory::secret_key(self.seed),
            self.discovery_direct_addrs.clone(),
            advertised_services,
            &handshake_config,
            self.limits.max_connections,
            0,
            supervisor.subscribe(),
        )?;
        let discovery_service =
            Arc::new(DiscoveryService::new(discovery.clone())) as Arc<dyn Service>;
        let registry = service_registry(&supervisor, base_service, discovery_service)?;
        let handler = ZakuraProtocolHandler::new_with_registry_and_trace(
            supervisor.clone(),
            network.clone(),
            handshake_config,
            self.limits.clone(),
            registry,
            ZakuraTrace::new(self.tracer.clone(), seed_label(self.seed)),
        );
        let router = Router::builder(endpoint)
            .accept(P2P_V2_ALPN, handler.clone())
            .spawn();
        let endpoint = ZakuraEndpoint::from_parts(router, supervisor, handler);

        Ok(ZakuraTestNode {
            seed: self.seed,
            endpoint,
            discovery,
            limits: self.limits,
            recorder,
            dial_tasks: Arc::new(Mutex::new(Vec::new())),
            _tracer: self.tracer,
        })
    }
}

fn seed_label(seed: u64) -> String {
    format!("{seed:02}")
}

impl Drop for ZakuraTestNode {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.dial_tasks.try_lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn legacy_upgrade_builder_fails_loudly() {
        let error = ZakuraTestNode::builder(1)
            .enable_legacy_upgrade(true)
            .spawn()
            .await
            .expect_err("legacy-upgrade hook is reserved, not silently ignored");

        assert!(error.to_string().contains("connect_via_upgrade"));
    }
}
