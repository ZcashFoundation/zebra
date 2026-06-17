//! Multi-node in-process Zakura harness.

use std::time::Duration;

use super::{await_until, TraceCapture, ZakuraTestNode};
use crate::{zakura::ZakuraPeerId, BoxError};

/// Supported deterministic topologies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClusterTopology {
    /// Every node dials every higher-indexed peer.
    FullMesh,
    /// Node `n` dials node `n + 1`.
    Line,
}

/// In-process collection of Zakura nodes.
#[derive(Debug, Default)]
pub struct ZakuraTestCluster {
    nodes: Vec<ZakuraTestNode>,
}

impl ZakuraTestCluster {
    /// Create an empty cluster.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn one node and append it to the cluster.
    pub async fn spawn_node(&mut self, seed: u64) -> Result<usize, BoxError> {
        let node = ZakuraTestNode::builder(seed).spawn().await?;
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    /// Spawn one node with a per-node JSONL trace directory.
    pub async fn spawn_traced_node(
        &mut self,
        seed: u64,
        trace: &mut TraceCapture,
    ) -> Result<usize, BoxError> {
        let node = ZakuraTestNode::builder(seed)
            .tracer(trace.tracer_for_node(seed))
            .spawn()
            .await?;
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    /// Spawn all nodes in `seeds`.
    pub async fn spawn_nodes(
        &mut self,
        seeds: impl IntoIterator<Item = u64>,
    ) -> Result<(), BoxError> {
        for seed in seeds {
            self.spawn_node(seed).await?;
        }
        Ok(())
    }

    /// Borrow all nodes.
    pub fn nodes(&self) -> &[ZakuraTestNode] {
        &self.nodes
    }

    /// Borrow one node by index.
    pub fn node(&self, index: usize) -> &ZakuraTestNode {
        &self.nodes[index]
    }

    /// Connect nodes according to `topology`.
    pub async fn connect_topology(
        &self,
        topology: ClusterTopology,
        timeout: Duration,
    ) -> Result<(), BoxError> {
        match topology {
            ClusterTopology::FullMesh => self.connect_full_mesh(timeout).await,
            ClusterTopology::Line => {
                for pair in self.nodes.windows(2) {
                    pair[0].connect_native(&pair[1], timeout).await?;
                }
                Ok(())
            }
        }
    }

    /// Connect every pair in the cluster.
    pub async fn connect_full_mesh(&self, timeout: Duration) -> Result<(), BoxError> {
        for left in 0..self.nodes.len() {
            for right in (left + 1)..self.nodes.len() {
                self.nodes[left]
                    .connect_native(&self.nodes[right], timeout)
                    .await?;
            }
        }
        Ok(())
    }

    /// Wait until every node has registered all expected peers.
    pub async fn await_all_connected(&self, timeout: Duration) -> Result<(), BoxError> {
        let mut ids = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            ids.push(node.node_addr().await.node_id.as_bytes().to_vec());
        }

        for node in &self.nodes {
            let own_id = node.node_addr().await.node_id.as_bytes().to_vec();
            let expected_peers: Vec<Vec<u8>> =
                ids.iter().filter(|id| **id != own_id).cloned().collect();
            let registered = node.supervisor().subscribe();
            await_until("cluster peer set", timeout, || {
                expected_peers
                    .iter()
                    .all(|expected| contains_peer(&registered.borrow(), expected))
            })
            .await?;
        }
        Ok(())
    }

    /// Shut down all nodes.
    pub async fn shutdown(&self) {
        for node in &self.nodes {
            node.shutdown().await;
        }
    }
}

fn contains_peer(peers: &[ZakuraPeerId], expected: &[u8]) -> bool {
    peers.iter().any(|peer| peer.as_bytes() == expected)
}

#[cfg(test)]
mod tests {
    use super::super::HostilePeer;
    use super::*;
    use crate::{
        zakura::{
            DiscoveryMessage, Frame, FramedSend, Peer, Service, Stream, ZakuraLocalLimits,
            ZAKURA_CAP_DISCOVERY, ZAKURA_CAP_LEGACY_GOSSIP, ZAKURA_STREAM_DISCOVERY,
            ZAKURA_STREAM_GOSSIP,
        },
        Config,
    };
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{mpsc, Mutex};

    #[derive(Debug, Default)]
    struct OrderedSourceProbeService {
        senders: Arc<Mutex<HashMap<ZakuraPeerId, FramedSend>>>,
    }

    impl OrderedSourceProbeService {
        async fn contains_peer(&self, peer: &ZakuraPeerId) -> bool {
            self.senders.lock().await.contains_key(peer)
        }

        async fn send_payload(
            &self,
            peer: &ZakuraPeerId,
            payload: Vec<u8>,
        ) -> Result<(), BoxError> {
            let sender = {
                let senders = self.senders.lock().await;
                senders.get(peer).cloned()
            };
            let Some(sender) = sender else {
                return Err("source probe peer sender missing".into());
            };
            sender
                .send(Frame {
                    message_type: 77,
                    flags: 0,
                    payload,
                })
                .await
                .map_err(|_| -> BoxError { "source probe sender closed".into() })
        }
    }

    impl Service for OrderedSourceProbeService {
        fn name(&self) -> &'static str {
            "ordered-source-probe"
        }

        fn streams(&self) -> &[Stream] {
            crate::zakura::legacy_gossip_streams()
        }

        fn add_peer(&self, mut peer: Peer) {
            let peer_id = peer.id.clone();
            let Some((mut recv, send)) = peer.take_stream(ZAKURA_STREAM_GOSSIP) else {
                return;
            };
            let cancel_token = peer.cancel_token();
            let senders = self.senders.clone();
            tokio::spawn(async move {
                senders.lock().await.insert(peer_id.clone(), send);
                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => break,
                        frame = recv.recv() => {
                            if frame.is_none() {
                                break;
                            }
                        }
                    }
                }
                senders.lock().await.remove(&peer_id);
            });
        }

        fn remove_peer(&self, peer: &ZakuraPeerId) {
            let senders = self.senders.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                senders.lock().await.remove(&peer);
            });
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum TaskExitProbeEvent {
        Added(ZakuraPeerId),
        SinkExited(ZakuraPeerId),
        SourceExited(ZakuraPeerId),
        Removed(ZakuraPeerId),
    }

    #[derive(Debug)]
    struct TaskExitProbeService {
        events: mpsc::UnboundedSender<TaskExitProbeEvent>,
    }

    impl TaskExitProbeService {
        fn new(events: mpsc::UnboundedSender<TaskExitProbeEvent>) -> Arc<Self> {
            Arc::new(Self { events })
        }
    }

    impl Service for TaskExitProbeService {
        fn name(&self) -> &'static str {
            "task-exit-probe"
        }

        fn streams(&self) -> &[Stream] {
            crate::zakura::legacy_gossip_streams()
        }

        fn add_peer(&self, mut peer: Peer) {
            let peer_id = peer.id.clone();
            let _ = self.events.send(TaskExitProbeEvent::Added(peer_id.clone()));
            let Some((mut recv, send)) = peer.take_stream(ZAKURA_STREAM_GOSSIP) else {
                return;
            };

            let cancel_token = peer.cancel_token();
            let sink_events = self.events.clone();
            let sink_peer = peer_id.clone();
            let sink_cancel = cancel_token.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = sink_cancel.cancelled() => {
                            let _ = sink_events.send(TaskExitProbeEvent::SinkExited(sink_peer));
                            return;
                        }
                        frame = recv.recv() => {
                            if frame.is_none() {
                                let _ = sink_events.send(TaskExitProbeEvent::SinkExited(sink_peer));
                                return;
                            }
                        }
                    }
                }
            });

            let source_events = self.events.clone();
            tokio::spawn(async move {
                let _send = send;
                cancel_token.cancelled().await;
                let _ = source_events.send(TaskExitProbeEvent::SourceExited(peer_id));
            });
        }

        fn remove_peer(&self, peer: &ZakuraPeerId) {
            let _ = self.events.send(TaskExitProbeEvent::Removed(peer.clone()));
        }
    }

    async fn wait_for_probe_event(
        events: &mut mpsc::UnboundedReceiver<TaskExitProbeEvent>,
        label: &'static str,
        mut matches: impl FnMut(&TaskExitProbeEvent) -> bool,
    ) -> Result<TaskExitProbeEvent, BoxError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.ok_or_else(|| -> BoxError {
                    format!("task-exit probe closed before {label}").into()
                })?;
                if matches(&event) {
                    return Ok(event);
                }
            }
        })
        .await
        .map_err(|_| -> BoxError { format!("timed out waiting for {label}").into() })?
    }

    #[tokio::test]
    #[ignore = "native handler mesh smoke is exercised by the zakura-integration nextest profile once dial scheduling is made deterministic"]
    async fn cluster_forms_native_two_node_mesh() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut cluster = ZakuraTestCluster::new();
        cluster.spawn_nodes([1, 2]).await?;

        cluster.connect_full_mesh(Duration::from_secs(5)).await?;
        cluster.await_all_connected(Duration::from_secs(5)).await?;
        cluster.shutdown().await;

        Ok(())
    }

    #[tokio::test]
    async fn traced_node_records_native_handshake_and_ratelimit_events() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "traced_node_records_native_handshake_and_ratelimit_events",
            false,
        )?;
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_traced_node(1, &mut capture).await?;
        let victim = cluster.node(victim_idx);
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 2, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;

        tokio::time::sleep(Duration::from_millis(200)).await;
        hostile.oversize_frame_declared_len(2).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        hostile.shutdown().await;
        cluster.shutdown().await;
        capture.flush().await;

        let reader = capture.reader()?;
        reader
            .node("01")
            .table("handshake")
            .assert_sequence(&["control.started", "control.succeeded"]);
        assert!(reader.node("01").table("conn").count("accepted") >= 1);
        assert!(reader.node("01").table("stream").count("accepted") >= 1);
        assert!(reader.node("01").table("ratelimit").count("frame.oversize") >= 1);

        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_stream_kind_is_reset_and_never_delivered() -> Result<(), BoxError> {
        // FLUP-015: a peer-controlled prelude naming an unknown kind must be
        // reset before the stream's frame reaches the inbound sink, while a
        // known kind on the same connection is still delivered. Asserted on
        // recorder state, not metrics.
        let _guard = zebra_test::init();
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_node(1).await?;
        let victim = cluster.node(victim_idx);
        let recorder = victim.recorder();
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 2, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;

        let known_payload = b"known-kind-frame".to_vec();
        let unknown_payload = b"unknown-kind-frame".to_vec();
        // Unknown kind 9: must be reset and dropped.
        hostile.send_frame(9, unknown_payload.clone()).await?;
        // Known kind 2 (gossip): must be delivered.
        hostile.send_frame(2, known_payload.clone()).await?;

        await_until("known-kind frame delivered", Duration::from_secs(5), || {
            recorder.contains_payload(2, &known_payload)
        })
        .await?;

        // The known frame arrived; the unknown one must never have been delivered
        // under any kind label.
        let delivered = recorder.drain();
        assert!(
            delivered
                .iter()
                .any(|m| m.stream_kind == 2 && m.frame.payload == known_payload),
            "known-kind frame must be delivered"
        );
        assert!(
            !delivered.iter().any(|m| m.frame.payload == unknown_payload),
            "unknown-kind frame must be reset before delivery, got {delivered:?}"
        );

        hostile.shutdown().await;
        cluster.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_stream_version_is_reset_and_never_delivered() -> Result<(), BoxError> {
        // FLUP-015: a known kind at an unsupported version is rejected too.
        let _guard = zebra_test::init();
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_node(3).await?;
        let victim = cluster.node(victim_idx);
        let recorder = victim.recorder();
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 4, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;

        let bad_version = b"kind-2-version-99".to_vec();
        let good = b"kind-2-version-1".to_vec();
        hostile
            .send_frame_with_version(2, 99, bad_version.clone())
            .await?;
        hostile.send_frame(2, good.clone()).await?;

        await_until("version-1 frame delivered", Duration::from_secs(5), || {
            recorder.contains_payload(2, &good)
        })
        .await?;

        let delivered = recorder.drain();
        assert!(
            !delivered.iter().any(|m| m.frame.payload == bad_version),
            "unsupported-version frame must be reset before delivery, got {delivered:?}"
        );

        hostile.shutdown().await;
        cluster.shutdown().await;
        Ok(())
    }

    /// Builds a discovery `GetPeers` request frame on the native wire.
    fn discovery_get_peers_frame() -> Frame {
        Frame {
            message_type: 1,
            flags: 0,
            payload: DiscoveryMessage::GetPeers {
                limit: 8,
                wanted_services: Vec::new(),
                exclude_node_ids: Vec::new(),
            }
            .encode()
            .expect("empty GetPeers encodes"),
        }
    }

    #[tokio::test]
    async fn discovery_stream_requires_negotiated_capability_and_responds() -> Result<(), BoxError>
    {
        let _guard = zebra_test::init();
        let victim = ZakuraTestNode::builder(26).spawn().await?;
        let victim_node_id = victim.node_addr().await.node_id;

        // A peer that did not negotiate the discovery capability cannot open a
        // discovery stream and receives no service response.
        let zero_cap_peer = HostilePeer::connect_native_with_capabilities(&victim, 27, 0).await?;
        zero_cap_peer
            .send_raw_frame(ZAKURA_STREAM_DISCOVERY, discovery_get_peers_frame())
            .await?;
        let rejected = tokio::time::timeout(
            Duration::from_millis(200),
            zero_cap_peer.recv_ordered_frame(ZAKURA_STREAM_DISCOVERY),
        )
        .await;
        assert!(
            rejected.is_err() || rejected.is_ok_and(|result| result.is_err()),
            "unnegotiated discovery stream must not receive a service response"
        );

        // A peer that negotiated discovery exchanges native discovery messages:
        // the victim gossips its own signed self-record (Hello) and answers our
        // GetPeers with a Peers response (empty, since it knows no other peers).
        let discovery_peer =
            HostilePeer::connect_native_with_capabilities(&victim, 28, ZAKURA_CAP_DISCOVERY)
                .await?;
        discovery_peer
            .send_raw_frame(ZAKURA_STREAM_DISCOVERY, discovery_get_peers_frame())
            .await?;

        let mut saw_hello = false;
        let mut saw_peers = false;
        for _ in 0..8 {
            if saw_hello && saw_peers {
                break;
            }
            let frame = tokio::time::timeout(
                Duration::from_secs(5),
                discovery_peer.recv_ordered_frame(ZAKURA_STREAM_DISCOVERY),
            )
            .await??;
            assert_eq!(frame.message_type, 1);
            assert_eq!(frame.flags, 0);
            match DiscoveryMessage::decode(&frame.payload)? {
                DiscoveryMessage::Hello { record } => {
                    assert_eq!(record.body.node_id, victim_node_id);
                    saw_hello = true;
                }
                DiscoveryMessage::Peers { records } => {
                    assert!(records.is_empty());
                    saw_peers = true;
                }
                // The victim's own discovery source also asks us for peers.
                DiscoveryMessage::GetPeers { .. } => {}
                other => panic!("unexpected discovery message: {other:?}"),
            }
        }
        assert!(saw_hello, "victim gossips its signed self-record");
        assert!(saw_peers, "victim answers GetPeers with a Peers response");

        zero_cap_peer.shutdown().await;
        discovery_peer.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn discovery_candidate_dialer_connects_static_candidate() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let dialer = ZakuraTestNode::builder(50).spawn().await?;
        let target = ZakuraTestNode::builder(51).spawn().await?;

        // Seed `target` as a trusted static candidate (loopback allowed) and let
        // the book-driven candidate dialer connect it.
        let target_id = dialer.insert_static_discovery_candidate(&target).await?;
        let _dialer_task = dialer.spawn_discovery_dialer();

        let peer_set = dialer.supervisor().subscribe();
        await_until(
            "discovery dialer connects the static candidate",
            Duration::from_secs(10),
            || contains_peer(&peer_set.borrow(), target_id.as_bytes()),
        )
        .await?;

        dialer.shutdown().await;
        target.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn connected_peers_import_each_others_signed_records() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        // Advertise dialable (non-loopback) addresses so the gossiped records are
        // kept in the dialable book rather than dropped as locally non-dialable.
        let addr_a = "203.0.113.10:9"
            .parse::<std::net::SocketAddr>()
            .expect("valid test addr");
        let addr_b = "203.0.113.11:9"
            .parse::<std::net::SocketAddr>()
            .expect("valid test addr");
        let a = ZakuraTestNode::builder(52)
            .discovery_direct_addrs(vec![addr_a])
            .spawn()
            .await?;
        let b = ZakuraTestNode::builder(53)
            .discovery_direct_addrs(vec![addr_b])
            .spawn()
            .await?;
        let b_id = b.node_addr().await.node_id;

        a.connect_native(&b, Duration::from_secs(5)).await?;

        let mut learned = false;
        for _ in 0..100 {
            if a.discovery().record_for(b_id).await.is_some() {
                learned = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(learned, "node a imports node b's gossiped self-record");

        a.shutdown().await;
        b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_discovery_frame_disconnects_negotiated_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let victim = ZakuraTestNode::builder(32).spawn().await?;
        let peer_set = victim.supervisor().subscribe();

        let discovery_peer =
            HostilePeer::connect_native_with_capabilities(&victim, 33, ZAKURA_CAP_DISCOVERY)
                .await?;
        let peer_id = discovery_peer.id()?;

        await_until("discovery peer registered", Duration::from_secs(5), || {
            contains_peer(&peer_set.borrow(), peer_id.as_bytes())
        })
        .await?;

        discovery_peer
            .send_raw_frame(
                ZAKURA_STREAM_DISCOVERY,
                Frame {
                    message_type: 99,
                    flags: 0,
                    payload: Vec::new(),
                },
            )
            .await?;

        await_until(
            "protocol-invalid discovery peer deregistered",
            Duration::from_secs(5),
            || !contains_peer(&peer_set.borrow(), peer_id.as_bytes()),
        )
        .await?;

        discovery_peer.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn discovery_stream_uses_transport_rate_and_oversize_bounds() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "discovery_stream_uses_transport_rate_and_oversize_bounds",
            false,
        )?;
        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = 16;
        limits.max_pending_handshakes = 8;
        limits.max_open_streams = 16;
        limits.max_inbound_queue_depth = 256;
        limits.message_rate_per_second = 1;
        limits.stream_open_rate_per_second = 64;
        let victim = ZakuraTestNode::builder(29)
            .limits(limits)
            .tracer(capture.tracer_for_node(29))
            .spawn()
            .await?;

        let flooding =
            HostilePeer::connect_native_with_capabilities(&victim, 30, ZAKURA_CAP_DISCOVERY)
                .await?;
        flooding
            .flood_stream(ZAKURA_STREAM_DISCOVERY, 'd', 16)
            .await?;
        await_until(
            "discovery throttling traced",
            Duration::from_secs(5),
            || {
                capture.reader().is_ok_and(|reader| {
                    reader
                        .node("29")
                        .table("ratelimit")
                        .rows()
                        .iter()
                        .any(|row| {
                            row.get("event").and_then(serde_json::Value::as_str)
                                == Some("message.throttled")
                                && row.get("stream_kind").and_then(serde_json::Value::as_str)
                                    == Some("discovery")
                        })
                })
            },
        )
        .await?;
        flooding.shutdown().await;

        let oversized =
            HostilePeer::connect_native_with_capabilities(&victim, 31, ZAKURA_CAP_DISCOVERY)
                .await?;
        oversized
            .oversize_frame_declared_len(ZAKURA_STREAM_DISCOVERY)
            .await?;
        await_until("discovery oversize traced", Duration::from_secs(5), || {
            capture.reader().is_ok_and(|reader| {
                reader
                    .node("29")
                    .table("ratelimit")
                    .rows()
                    .iter()
                    .any(|row| {
                        row.get("event").and_then(serde_json::Value::as_str)
                            == Some("frame.oversize")
                            && row.get("stream_kind").and_then(serde_json::Value::as_str)
                                == Some("discovery")
                    })
            })
        })
        .await?;

        oversized.shutdown().await;
        victim.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn persistent_ordered_stream_uses_message_budget() -> Result<(), BoxError> {
        // P2: a long-lived ordered stream still spends the transport-owned
        // per-kind message-rate budget before frames reach the service.
        let _guard = zebra_test::init();

        // Small, deterministic message budget so the aggregate cap is observable
        // without sending hundreds of frames.
        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = 16;
        limits.max_pending_handshakes = 8;
        limits.max_open_streams = 16;
        limits.max_inbound_queue_depth = 256;
        limits.message_rate_per_second = 4;
        // Allow the stream opens themselves (open-rate is a separate limiter).
        limits.stream_open_rate_per_second = 64;
        let message_budget = limits.message_rate_per_second as usize;

        let victim = ZakuraTestNode::builder(5).limits(limits).spawn().await?;
        let recorder = victim.recorder();
        let hostile =
            HostilePeer::connect_native_with_capabilities(&victim, 6, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;

        let sent = message_budget * 8;
        for index in 0..sent {
            hostile
                .send_frame(2, format!("a-{index}").into_bytes())
                .await?;
        }

        // Wait until rate limiting has clearly engaged (more frames sent than one
        // budget, so the bucket must have emptied at least once).
        await_until("rate limiting engaged", Duration::from_secs(5), || {
            recorder.len() + recorder.dropped_count() >= message_budget
        })
        .await?;
        // Brief deterministic settle to let one refill window pass. A correct
        // per-kind bucket should remain close to the initial burst plus one
        // refill, not merely below the much larger flood size.
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Total ever delivered = retained + dropped-by-recorder (the recorder is
        // a bounded tap). The bucket caps the burst near one budget even though
        // the peer sent many frames on the persistent stream.
        let delivered_total = recorder.len() + recorder.dropped_count();
        assert!(
            delivered_total <= message_budget * 2,
            "persistent stream flood delivered {delivered_total} of {sent} frames; \
             the per-kind message bucket must throttle the peer"
        );

        hostile.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn persistent_ordered_stream_delivers_frames_in_order() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let victim = ZakuraTestNode::builder(16).spawn().await?;
        let recorder = victim.recorder();
        let hostile =
            HostilePeer::connect_native_with_capabilities(&victim, 17, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;
        let payloads: Vec<Vec<u8>> = (0..4)
            .map(|index| format!("ordered-{index}").into_bytes())
            .collect();

        for payload in &payloads {
            hostile
                .send_frame(ZAKURA_STREAM_GOSSIP, payload.clone())
                .await?;
        }

        await_until(
            "ordered gossip burst delivered",
            Duration::from_secs(5),
            || recorder.len() >= payloads.len(),
        )
        .await?;
        let delivered: Vec<_> = recorder
            .drain()
            .into_iter()
            .filter(|message| message.stream_kind == ZAKURA_STREAM_GOSSIP)
            .map(|message| message.frame.payload)
            .collect();

        assert_eq!(delivered, payloads);

        hostile.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn service_owned_source_sends_multiple_ordered_frames() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let service = Arc::new(OrderedSourceProbeService::default());
        let victim = ZakuraTestNode::builder(24)
            .service(service.clone())
            .spawn()
            .await?;
        let hostile =
            HostilePeer::connect_native_with_capabilities(&victim, 25, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;
        let peer_id = hostile.id()?;

        hostile
            .send_frame(ZAKURA_STREAM_GOSSIP, b"open-source-stream".to_vec())
            .await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !service.contains_peer(&peer_id).await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| -> BoxError { "source probe peer registration timed out".into() })?;

        let first = b"source-one".to_vec();
        let second = b"source-two".to_vec();
        service.send_payload(&peer_id, first.clone()).await?;
        service.send_payload(&peer_id, second.clone()).await?;

        let received_first = tokio::time::timeout(
            Duration::from_secs(5),
            hostile.recv_ordered_frame(ZAKURA_STREAM_GOSSIP),
        )
        .await??;
        let received_second = tokio::time::timeout(
            Duration::from_secs(5),
            hostile.recv_ordered_frame(ZAKURA_STREAM_GOSSIP),
        )
        .await??;

        assert_eq!(received_first.payload, first);
        assert_eq!(received_second.payload, second);

        hostile.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn single_peer_disconnect_cancels_service_stream_tasks() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let victim = ZakuraTestNode::builder(21)
            .service(TaskExitProbeService::new(events_tx))
            .spawn()
            .await?;
        let hostile =
            HostilePeer::connect_native_with_capabilities(&victim, 22, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;
        let peer_id = hostile.id()?;

        hostile
            .send_frame(ZAKURA_STREAM_GOSSIP, b"start-probe".to_vec())
            .await?;
        wait_for_probe_event(
            &mut events_rx,
            "service add",
            |event| matches!(event, TaskExitProbeEvent::Added(peer) if peer == &peer_id),
        )
        .await?;

        assert!(
            victim.supervisor().disconnect_peer(&peer_id).await,
            "the hostile peer should be registered before disconnect"
        );

        let mut sink_exited = false;
        let mut source_exited = false;
        while !sink_exited || !source_exited {
            match wait_for_probe_event(&mut events_rx, "service task exit", |event| {
                matches!(
                    event,
                    TaskExitProbeEvent::SinkExited(peer)
                        | TaskExitProbeEvent::SourceExited(peer)
                        if peer == &peer_id
                )
            })
            .await?
            {
                TaskExitProbeEvent::SinkExited(peer) if peer == peer_id => sink_exited = true,
                TaskExitProbeEvent::SourceExited(peer) if peer == peer_id => source_exited = true,
                _ => {}
            }
        }

        wait_for_probe_event(
            &mut events_rx,
            "service remove",
            |event| matches!(event, TaskExitProbeEvent::Removed(peer) if peer == &peer_id),
        )
        .await?;

        let second =
            HostilePeer::connect_native_with_capabilities(&victim, 23, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;
        second.shutdown().await;
        hostile.shutdown().await;
        victim.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn impossible_ordered_stream_limits_do_not_leave_registered_peer() -> Result<(), BoxError>
    {
        let _guard = zebra_test::init();
        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = 4;
        limits.max_pending_handshakes = 4;
        limits.max_open_streams = 16;
        limits.max_inbound_queue_depth = 1;
        let victim = ZakuraTestNode::builder(18).limits(limits).spawn().await?;
        let peer_set = victim.supervisor().subscribe();

        let first = HostilePeer::connect_native(&victim, 19).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            peer_set.borrow().is_empty(),
            "peer rejected before registration must not remain in the supervisor peer set"
        );
        if let Ok(first) = first {
            first.shutdown().await;
        }

        let second = HostilePeer::connect_native(&victim, 20).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            peer_set.borrow().is_empty(),
            "a later peer must not be rejected because stale registration state was leaked"
        );
        if let Ok(second) = second {
            second.shutdown().await;
        }
        victim.shutdown().await;
        Ok(())
    }
}
