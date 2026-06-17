//! Registry for Zakura protocol services and their declared streams.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use thiserror::Error;

use super::{Frame, Peer, Service, SinkReject, Stream, StreamMode};
use crate::zakura::{ServicePeerDirection, ZakuraPeerId};

/// Errors returned while building a [`ServiceRegistry`].
#[derive(Debug, Error)]
pub enum RegistryError {
    /// Two services declared the same stream kind.
    #[error(
        "duplicate Zakura stream kind {kind} declared by {first_service} and {second_service}"
    )]
    DuplicateKind {
        /// Duplicated stream kind.
        kind: u16,
        /// Service that declared the kind first.
        first_service: &'static str,
        /// Service that declared the kind again.
        second_service: &'static str,
    },

    /// A service declared a stream whose capability is not exactly one bit.
    ///
    /// Each [`Stream`] maps to a single capability bit so
    /// that `supported_capabilities()` (an OR of every declared bit) stays
    /// consistent with per-bit `services_for_capability()` lookups and the P1
    /// add-peer fan-out. A zero or multi-bit capability would make those two
    /// views disagree, so it is rejected at registry-build time.
    #[error(
        "service {service} declared stream kind {kind} with capability {capability:#x}, \
         which must be a single non-zero bit"
    )]
    InvalidCapability {
        /// Service that declared the stream.
        service: &'static str,
        /// Stream kind carrying the invalid capability.
        kind: u16,
        /// The invalid capability value.
        capability: u64,
    },
}

/// Registry of Zakura protocol services.
#[derive(Clone, Debug, Default)]
pub struct ServiceRegistry {
    services: Vec<Arc<dyn Service>>,
    by_kind: HashMap<u16, usize>,
    by_capability: HashMap<u64, Vec<usize>>,
    supported_capabilities: u64,
}

impl ServiceRegistry {
    /// Build a registry from protocol services.
    pub fn new(services: Vec<Arc<dyn Service>>) -> Result<Self, RegistryError> {
        let mut by_kind = HashMap::new();
        let mut by_capability: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut supported_capabilities = 0;

        for (index, service) in services.iter().enumerate() {
            let mut service_capabilities = HashSet::new();

            for stream in service.streams() {
                // Each stream must map to exactly one capability bit, otherwise
                // `supported_capabilities` (the OR below) and per-bit
                // `services_for_capability` lookups disagree.
                if stream.capability == 0 || !stream.capability.is_power_of_two() {
                    return Err(RegistryError::InvalidCapability {
                        service: service.name(),
                        kind: stream.kind,
                        capability: stream.capability,
                    });
                }

                if let Some(first_index) = by_kind.insert(stream.kind, index) {
                    return Err(RegistryError::DuplicateKind {
                        kind: stream.kind,
                        first_service: services[first_index].name(),
                        second_service: service.name(),
                    });
                }

                supported_capabilities |= stream.capability;
                service_capabilities.insert(stream.capability);
            }

            for capability in service_capabilities {
                by_capability.entry(capability).or_default().push(index);
            }
        }

        Ok(Self {
            services,
            by_kind,
            by_capability,
            supported_capabilities,
        })
    }

    /// Return every registered service in insertion order.
    pub fn services(&self) -> &[Arc<dyn Service>] {
        &self.services
    }

    /// Lookup the service that owns `kind`.
    pub fn service_for_kind(&self, kind: u16) -> Option<Arc<dyn Service>> {
        self.by_kind
            .get(&kind)
            .map(|index| Arc::clone(&self.services[*index]))
    }

    /// Lookup the single capability bit for `kind` and `version`.
    pub fn capability_for_stream(&self, kind: u16, version: u16) -> Option<u64> {
        self.stream(kind, version).map(|stream| stream.capability)
    }

    /// Lookup a declared stream by kind and version.
    pub fn stream(&self, kind: u16, version: u16) -> Option<Stream> {
        let service = self.service_for_kind(kind)?;
        service
            .streams()
            .iter()
            .find(|stream| stream.kind == kind && stream.version == version)
            .copied()
    }

    /// Returns true when a registered service owns `kind` at `version`.
    pub fn is_supported_stream(&self, kind: u16, version: u16) -> bool {
        self.capability_for_stream(kind, version).is_some()
    }

    /// Lookup services that declared `capability`.
    pub fn services_for_capability(&self, capability: u64) -> Vec<Arc<dyn Service>> {
        self.by_capability
            .get(&capability)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .map(|index| Arc::clone(&self.services[*index]))
            .collect()
    }

    /// Lookup services that declared any capability bit in `negotiated`.
    ///
    /// Each service is returned once, in registration order, even if multiple of
    /// its declared streams match the negotiated capability mask.
    pub fn services_for_negotiated(&self, negotiated: u64) -> Vec<Arc<dyn Service>> {
        let mut matched_indexes = HashSet::new();
        let mut remaining = negotiated & self.supported_capabilities;

        while remaining != 0 {
            let capability = 1_u64 << remaining.trailing_zeros();
            remaining &= !capability;

            if let Some(indexes) = self.by_capability.get(&capability) {
                matched_indexes.extend(indexes.iter().copied());
            }
        }

        self.services
            .iter()
            .enumerate()
            .filter(|(index, _service)| matched_indexes.contains(index))
            .map(|(_index, service)| Arc::clone(service))
            .collect()
    }

    /// OR of every stream capability declared by registered services.
    pub fn supported_capabilities(&self) -> u64 {
        self.supported_capabilities
    }

    /// Ordered streams negotiated with a peer, in registry service order.
    pub fn ordered_streams_for_negotiated(&self, negotiated: u64) -> Vec<Stream> {
        let mut streams = Vec::new();

        for service in self.services_for_negotiated(negotiated) {
            streams.extend(
                service
                    .streams()
                    .iter()
                    .copied()
                    .filter(|stream| stream.mode == StreamMode::Ordered),
            );
        }

        streams
    }

    /// Ordered streams that should be lazily escalated for this peer now.
    ///
    /// The connection initiator is the only side that proactively opens ordered
    /// service streams. This demand check narrows the negotiated capabilities to
    /// services that currently have local interest and room; the owning reactor
    /// still makes the final admission decision after the typed session arrives.
    pub fn ordered_streams_for_escalation(
        &self,
        negotiated: u64,
        peer_id: &ZakuraPeerId,
        direction: ServicePeerDirection,
    ) -> Vec<Stream> {
        let mut streams = Vec::new();

        for service in self.services_for_negotiated(negotiated) {
            if !service.wants_peer(peer_id, negotiated, direction) {
                continue;
            }

            streams.extend(
                service
                    .streams()
                    .iter()
                    .copied()
                    .filter(|stream| stream.mode == StreamMode::Ordered),
            );
        }

        streams
    }

    /// Return true when the service owning `kind` still wants this peer.
    pub fn wants_ordered_stream(
        &self,
        kind: u16,
        negotiated: u64,
        peer_id: &ZakuraPeerId,
        direction: ServicePeerDirection,
    ) -> bool {
        let Some(service) = self.service_for_kind(kind) else {
            return false;
        };

        service.wants_peer(peer_id, negotiated, direction)
    }

    /// Request/response streams negotiated with a peer, in registry service order.
    pub fn request_response_streams_for_negotiated(&self, negotiated: u64) -> Vec<Stream> {
        let mut streams = Vec::new();

        for service in self.services_for_negotiated(negotiated) {
            streams.extend(
                service
                    .streams()
                    .iter()
                    .copied()
                    .filter(|stream| stream.mode == StreamMode::RequestResponse),
            );
        }

        streams
    }

    /// Dispatch one test/recorder frame to the service that owns `kind`.
    pub fn deliver(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        let Some(service) = self.service_for_kind(stream_kind) else {
            return Ok(());
        };

        service.deliver_frame(peer_id, stream_kind, frame)
    }

    /// Dispatch one request-response frame to the service that owns `kind`.
    pub async fn request(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        request_id: u64,
        max_frame_bytes: u32,
        max_message_bytes: u32,
        frame: Frame,
    ) -> Result<Vec<Frame>, SinkReject> {
        let Some(service) = self.service_for_kind(stream_kind) else {
            return Err(SinkReject::protocol(
                "request stream kind is not registered",
            ));
        };
        let Some(handler) = service.as_request_response() else {
            return Err(SinkReject::protocol(
                "service does not accept request frames",
            ));
        };

        handler
            .request_frame(
                peer_id,
                stream_kind,
                request_id,
                max_frame_bytes,
                max_message_bytes,
                frame,
            )
            .await
    }

    /// Fan a newly connected peer out to every service enabled by its negotiated capabilities.
    pub fn add_peer(&self, peer: Peer) {
        let (peer_id, remote_ip, negotiated, direction, mut streams, cancel_token) =
            peer.into_parts();

        for service in self.services_for_negotiated(negotiated) {
            let service_streams: HashMap<_, _> = service
                .streams()
                .iter()
                .filter_map(|stream| {
                    streams
                        .remove(&stream.kind)
                        .map(|handles| (stream.kind, handles))
                })
                .collect();
            let service_cancel_token = service_streams
                .values()
                .next()
                .map(|stream| stream.cancel_token.clone())
                .unwrap_or_else(|| cancel_token.child_token());

            service.add_peer(Peer::new_with_service_cancel_token(
                peer_id.clone(),
                remote_ip,
                negotiated,
                direction,
                service_streams,
                cancel_token.clone(),
                service_cancel_token,
            ));
        }
    }

    /// Fan a lazily escalated peer out only to services with opened streams.
    ///
    /// Returns the capability mask for services that received a peer session, so
    /// disconnect fanout can be limited to reactors that were actually reached.
    pub fn add_escalated_peer(&self, peer: Peer) -> u64 {
        let (peer_id, remote_ip, negotiated, direction, mut streams, cancel_token) =
            peer.into_parts();
        let mut admitted_capabilities = 0;

        for service in self.services_for_negotiated(negotiated) {
            let service_streams: HashMap<_, _> = service
                .streams()
                .iter()
                .filter_map(|stream| {
                    streams.remove(&stream.kind).map(|handles| {
                        admitted_capabilities |= stream.capability;
                        (stream.kind, handles)
                    })
                })
                .collect();

            if service_streams.is_empty() {
                continue;
            }

            let service_cancel_token = service_streams
                .values()
                .next()
                .map(|stream| stream.cancel_token.clone())
                .unwrap_or_else(|| cancel_token.child_token());

            service.add_peer(Peer::new_with_service_cancel_token(
                peer_id.clone(),
                remote_ip,
                negotiated,
                direction,
                service_streams,
                cancel_token.clone(),
                service_cancel_token,
            ));
        }

        admitted_capabilities
    }

    /// Fan a disconnected peer out to every service enabled by `negotiated`.
    pub fn remove_peer(&self, peer_id: &ZakuraPeerId, negotiated: u64) {
        for service in self.services_for_negotiated(negotiated) {
            service.remove_peer(peer_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::zakura::{framed_channel, Peer, Stream, StreamMode, ZakuraPeerId};

    #[derive(Debug)]
    struct TestService {
        name: &'static str,
        streams: Vec<Stream>,
        wants: Mutex<bool>,
        added: Mutex<Vec<ZakuraPeerId>>,
        added_streams: Mutex<Vec<Vec<u16>>>,
        removed: Mutex<Vec<ZakuraPeerId>>,
    }

    impl TestService {
        fn new(name: &'static str, streams: Vec<Stream>) -> Arc<Self> {
            Arc::new(Self {
                name,
                streams,
                wants: Mutex::new(true),
                added: Mutex::new(Vec::new()),
                added_streams: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
            })
        }

        fn set_wants(&self, wants: bool) {
            *self
                .wants
                .lock()
                .expect("test service demand flag should not be poisoned") = wants;
        }
    }

    impl Service for TestService {
        fn name(&self) -> &'static str {
            self.name
        }

        fn streams(&self) -> &[Stream] {
            &self.streams
        }

        fn wants_peer(
            &self,
            _peer: &ZakuraPeerId,
            _negotiated: u64,
            _direction: ServicePeerDirection,
        ) -> bool {
            *self
                .wants
                .lock()
                .expect("test service demand flag should not be poisoned")
        }

        fn add_peer(&self, peer: Peer) {
            let (peer_id, _remote_ip, _negotiated, _direction, streams, _cancel_token) =
                peer.into_parts();
            self.added
                .lock()
                .expect("test service added list should not be poisoned")
                .push(peer_id);
            let mut stream_kinds: Vec<_> = streams.keys().copied().collect();
            stream_kinds.sort_unstable();
            self.added_streams
                .lock()
                .expect("test service stream list should not be poisoned")
                .push(stream_kinds);
        }

        fn remove_peer(&self, peer: &ZakuraPeerId) {
            self.removed
                .lock()
                .expect("test service removed list should not be poisoned")
                .push(peer.clone());
        }

        fn deliver_frame(
            &self,
            peer_id: ZakuraPeerId,
            _stream_kind: u16,
            _frame: Frame,
        ) -> Result<(), SinkReject> {
            self.added
                .lock()
                .map_err(|_| SinkReject::local("test service added list should not be poisoned"))?
                .push(peer_id);
            Ok(())
        }
    }

    fn stream(kind: u16, capability: u64) -> Stream {
        Stream {
            kind,
            version: 1,
            frame_cap: 1024,
            capability,
            mode: StreamMode::Ordered,
        }
    }

    #[test]
    fn registry_builds_kind_and_capability_lookups() {
        let header = TestService::new("header", vec![stream(5, 0b0001), stream(6, 0b0010)]);
        let gossip = TestService::new("gossip", vec![stream(2, 0b0100)]);

        let registry = ServiceRegistry::new(vec![header.clone(), gossip.clone()])
            .expect("test services declare unique stream kinds");

        assert_eq!(registry.services().len(), 2);
        assert_eq!(
            registry
                .service_for_kind(5)
                .expect("kind 5 is registered")
                .name(),
            "header"
        );
        assert_eq!(
            registry
                .service_for_kind(2)
                .expect("kind 2 is registered")
                .name(),
            "gossip"
        );
        assert!(registry.service_for_kind(99).is_none());
        assert_eq!(registry.services_for_capability(0b0010)[0].name(), "header");
        assert_eq!(registry.services_for_capability(0b0100)[0].name(), "gossip");
        assert!(registry.services_for_capability(0b1000).is_empty());
    }

    #[test]
    fn registry_rejects_duplicate_kinds() {
        let first = TestService::new("first", vec![stream(5, 0b0001)]);
        let second = TestService::new("second", vec![stream(5, 0b0010)]);

        let error = ServiceRegistry::new(vec![first, second])
            .expect_err("duplicate stream kinds must be rejected");

        assert!(matches!(
            error,
            RegistryError::DuplicateKind {
                kind: 5,
                first_service: "first",
                second_service: "second"
            }
        ));
    }

    #[test]
    fn registry_rejects_zero_capability() {
        let service = TestService::new("zero", vec![stream(5, 0)]);

        let error =
            ServiceRegistry::new(vec![service]).expect_err("a zero capability must be rejected");

        assert!(matches!(
            error,
            RegistryError::InvalidCapability {
                service: "zero",
                kind: 5,
                capability: 0
            }
        ));
    }

    #[test]
    fn registry_rejects_multi_bit_capability() {
        let service = TestService::new("multi", vec![stream(5, 0b0011)]);

        let error = ServiceRegistry::new(vec![service])
            .expect_err("a multi-bit capability must be rejected");

        assert!(matches!(
            error,
            RegistryError::InvalidCapability {
                service: "multi",
                kind: 5,
                capability: 0b0011
            }
        ));
    }

    #[test]
    fn supported_capabilities_are_or_of_declared_streams() {
        let header = TestService::new("header", vec![stream(5, 0b0001), stream(6, 0b0010)]);
        let gossip = TestService::new("gossip", vec![stream(2, 0b0100)]);

        let registry = ServiceRegistry::new(vec![header, gossip])
            .expect("test services declare unique stream kinds");

        assert_eq!(registry.supported_capabilities(), 0b0111);
    }

    #[test]
    fn services_for_negotiated_matches_any_bit_once_in_registration_order() {
        let header = TestService::new("header", vec![stream(5, 0b0001), stream(6, 0b0010)]);
        let gossip = TestService::new("gossip", vec![stream(2, 0b0100)]);
        let discovery = TestService::new("discovery", vec![stream(4, 0b1000)]);

        let registry = ServiceRegistry::new(vec![header, gossip, discovery])
            .expect("test services declare unique stream kinds");

        let services = registry.services_for_negotiated(0b1011);
        let service_names: Vec<_> = services.iter().map(|service| service.name()).collect();

        assert_eq!(service_names, ["header", "discovery"]);
    }

    #[test]
    fn add_peer_only_fires_for_negotiated_services_and_remove_frees_state() {
        let header = TestService::new("header", vec![stream(5, 0b0001)]);
        let gossip = TestService::new("gossip", vec![stream(2, 0b0010)]);
        let discovery = TestService::new("discovery", vec![stream(4, 0b0100)]);
        let registry =
            ServiceRegistry::new(vec![header.clone(), gossip.clone(), discovery.clone()])
                .expect("test services declare unique stream kinds");
        let peer = ZakuraPeerId::new(vec![9; 32]).expect("32-byte test peer id is valid");

        registry.add_peer(Peer::new(
            peer.clone(),
            None,
            0b0011,
            HashMap::new(),
            CancellationToken::new(),
        ));
        registry.remove_peer(&peer, 0b0011);

        assert_eq!(
            header
                .added
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            std::slice::from_ref(&peer)
        );
        assert_eq!(
            gossip
                .added
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            std::slice::from_ref(&peer)
        );
        assert!(discovery
            .added
            .lock()
            .expect("test mutex should not be poisoned")
            .is_empty());
        assert_eq!(
            header
                .removed
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            std::slice::from_ref(&peer)
        );
        assert_eq!(
            gossip
                .removed
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            &[peer]
        );
        assert!(discovery
            .removed
            .lock()
            .expect("test mutex should not be poisoned")
            .is_empty());
    }

    #[test]
    fn add_peer_fans_multi_stream_service_once_with_all_streams() {
        let header = TestService::new("header", vec![stream(5, 0b0001), stream(6, 0b0001)]);
        let registry = ServiceRegistry::new(vec![header.clone()]).expect("stream kinds are unique");
        let peer = ZakuraPeerId::new(vec![10; 32]).expect("32-byte test peer id is valid");
        let (send_5, recv_5) = framed_channel(1);
        let (send_6, recv_6) = framed_channel(1);
        let streams = HashMap::from([(5, (recv_5, send_5)), (6, (recv_6, send_6))]);

        registry.add_peer(Peer::new(
            peer.clone(),
            None,
            0b0001,
            streams,
            CancellationToken::new(),
        ));
        registry.remove_peer(&peer, 0b0001);

        assert_eq!(
            header
                .added
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            std::slice::from_ref(&peer)
        );
        assert_eq!(
            header
                .added_streams
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            &[vec![5, 6]]
        );
        assert_eq!(
            header
                .removed
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            &[peer]
        );
    }

    #[test]
    fn ordered_streams_for_escalation_filters_services_without_demand() {
        let header = TestService::new("header", vec![stream(5, 0b0001)]);
        let discovery = TestService::new("discovery", vec![stream(4, 0b0010)]);
        let registry = ServiceRegistry::new(vec![header.clone(), discovery.clone()])
            .expect("test services declare unique stream kinds");
        let peer = ZakuraPeerId::new(vec![11; 32]).expect("32-byte test peer id is valid");

        header.set_wants(false);

        let streams =
            registry.ordered_streams_for_escalation(0b0011, &peer, ServicePeerDirection::Outbound);
        let stream_kinds: Vec<_> = streams.iter().map(|stream| stream.kind).collect();

        assert_eq!(stream_kinds, [4]);
    }

    #[test]
    fn add_escalated_peer_fans_out_only_opened_streams_and_returns_remove_mask() {
        let header = TestService::new("header", vec![stream(5, 0b0001)]);
        let discovery = TestService::new("discovery", vec![stream(4, 0b0010)]);
        let registry = ServiceRegistry::new(vec![header.clone(), discovery.clone()])
            .expect("test services declare unique stream kinds");
        let peer = ZakuraPeerId::new(vec![12; 32]).expect("32-byte test peer id is valid");
        let (send_4, recv_4) = framed_channel(1);
        let streams = HashMap::from([(4, (recv_4, send_4))]);

        let remove_mask = registry.add_escalated_peer(Peer::new(
            peer.clone(),
            None,
            0b0010,
            streams,
            CancellationToken::new(),
        ));
        registry.remove_peer(&peer, remove_mask);

        assert!(header
            .added
            .lock()
            .expect("test mutex should not be poisoned")
            .is_empty());
        assert_eq!(
            discovery
                .added_streams
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            &[vec![4]]
        );
        assert_eq!(remove_mask, 0b0010);
        assert!(header
            .removed
            .lock()
            .expect("test mutex should not be poisoned")
            .is_empty());
        assert_eq!(
            discovery
                .removed
                .lock()
                .expect("test mutex should not be poisoned")
                .as_slice(),
            &[peer]
        );
    }
}
