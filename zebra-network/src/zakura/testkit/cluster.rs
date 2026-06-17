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
    use super::super::{trace_reader::TraceValue, HostilePeer, WaitError};
    use super::*;
    use crate::{
        zakura::trace::{block_sync_trace as bs_trace, header_sync_trace as hs_trace},
        zakura::{
            block_sync::{MAX_BS_FRAME_BYTES, ZAKURA_CAP_BLOCK_SYNC, ZAKURA_STREAM_BLOCK_SYNC},
            spawn_header_sync_reactor, BlockApplyResult, BlockSizeEstimate, BlockSyncAction,
            BlockSyncBlockMeta, BlockSyncEvent, BlockSyncFrontiers, BlockSyncMessage,
            BlockSyncStatus, DiscoveryMessage, Frame, FramedRecv, FramedSend, HeaderSyncAction,
            HeaderSyncCommitFailureKind, HeaderSyncEvent, HeaderSyncFrontiers, HeaderSyncHandle,
            HeaderSyncMessage, HeaderSyncMisbehavior, HeaderSyncPeerSession, HeaderSyncStartup,
            HeaderSyncStatus, Peer, Service, ServicePeerLimits, Stream, ZakuraBlockSyncConfig,
            ZakuraHeaderSyncConfig, ZakuraLocalLimits, ZakuraTrace, MAX_BS_RESPONSE_BYTES,
            ZAKURA_CAP_DISCOVERY, ZAKURA_CAP_HEADER_SYNC, ZAKURA_CAP_LEGACY_GOSSIP,
            ZAKURA_STREAM_DISCOVERY, ZAKURA_STREAM_GOSSIP, ZAKURA_STREAM_HEADER_SYNC,
        },
        Config,
    };
    use std::{
        collections::{BTreeMap, HashMap, HashSet},
        sync::{Arc, Mutex as StdMutex},
    };
    use tokio::{
        sync::{mpsc, Mutex},
        task::JoinHandle,
    };
    use tokio_util::sync::CancellationToken;
    use zebra_chain::{
        block,
        parameters::{
            testnet::{
                ConfiguredActivationHeights, ConfiguredCheckpoints, Parameters as TestnetParameters,
            },
            Network,
        },
        serialization::{ZcashDeserializeInto, ZcashSerialize},
    };
    use zebra_test::vectors::{
        BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES, BLOCK_MAINNET_3_BYTES, BLOCK_MAINNET_4_BYTES,
        BLOCK_MAINNET_5_BYTES, BLOCK_MAINNET_GENESIS_BYTES,
    };

    fn headers_message(headers: Vec<Arc<block::Header>>) -> HeaderSyncMessage {
        let body_sizes = vec![0; headers.len()];
        HeaderSyncMessage::Headers {
            headers,
            body_sizes,
        }
    }

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

    #[derive(Debug)]
    struct E2eHeaderStore {
        headers: BTreeMap<block::Height, (block::Hash, Arc<block::Header>)>,
        bodies: HashSet<block::Hash>,
        finalized_height: block::Height,
        verified_block_tip: block::Height,
        reject_next_commit: Option<HeaderSyncCommitFailureKind>,
    }

    impl E2eHeaderStore {
        fn genesis_only() -> Self {
            let genesis = mainnet_block(&BLOCK_MAINNET_GENESIS_BYTES);
            let mut headers = BTreeMap::new();
            headers.insert(block::Height(0), (genesis.hash(), genesis.header.clone()));
            Self {
                headers,
                bodies: HashSet::from([genesis.hash()]),
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                reject_next_commit: None,
            }
        }

        fn with_headers(up_to: u32) -> Self {
            let mut store = Self::genesis_only();
            for height in 1..=up_to {
                let block = mainnet_block(block_bytes(height));
                store
                    .headers
                    .insert(block::Height(height), (block.hash(), block.header.clone()));
            }
            store
        }

        fn with_checkpoint_anchor(height: u32) -> Self {
            let mut store = Self::genesis_only();
            let block = mainnet_block(block_bytes(height));
            store
                .headers
                .insert(block::Height(height), (block.hash(), block.header.clone()));
            store.finalized_height = block::Height(height);
            store.verified_block_tip = block::Height(0);
            store
        }

        fn best_header_tip(&self) -> (block::Height, block::Hash) {
            self.headers
                .last_key_value()
                .map(|(height, (hash, _))| (*height, *hash))
                .expect("test stores always contain genesis")
        }

        fn frontiers(&self) -> HeaderSyncFrontiers {
            let verified_block_hash = self
                .headers
                .get(&self.verified_block_tip)
                .map(|(hash, _)| *hash)
                .expect("verified test frontier has a known header");
            HeaderSyncFrontiers {
                finalized_height: self.finalized_height,
                verified_block_tip: self.verified_block_tip,
                verified_block_hash,
            }
        }

        fn headers_by_range(&self, start: block::Height, count: u32) -> Vec<Arc<block::Header>> {
            let mut headers = Vec::new();
            for offset in 0..count {
                let Some(height) = start + i64::from(offset) else {
                    break;
                };
                let Some((_hash, header)) = self.headers.get(&height) else {
                    break;
                };
                headers.push(header.clone());
            }
            headers
        }

        fn commit_headers(
            &mut self,
            anchor: block::Hash,
            start: block::Height,
            headers: Vec<Arc<block::Header>>,
            finalized: bool,
        ) -> Result<(block::Height, block::Hash), HeaderSyncCommitFailureKind> {
            if let Some(kind) = self.reject_next_commit.take() {
                return Err(kind);
            }

            let mut expected_previous = anchor;
            for (offset, header) in headers.iter().enumerate() {
                if header.previous_block_hash != expected_previous {
                    return Err(HeaderSyncCommitFailureKind::InvalidPeerRange);
                }
                let height = (start
                    + i64::try_from(offset)
                        .map_err(|_| HeaderSyncCommitFailureKind::InvalidPeerRange)?)
                .ok_or(HeaderSyncCommitFailureKind::InvalidPeerRange)?;
                let hash = block::Hash::from(header.as_ref());
                self.headers.insert(height, (hash, header.clone()));
                expected_previous = hash;
            }

            let count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
            let tip_height = block::Height(start.0.saturating_add(count.saturating_sub(1)));
            let tip_hash = expected_previous;
            if finalized {
                self.finalized_height = self.finalized_height.max(tip_height);
            }

            Ok((tip_height, tip_hash))
        }

        fn commit_body(&mut self, block: Arc<block::Block>) {
            let height = block.coinbase_height().expect("test block has height");
            let hash = block.hash();
            self.headers.insert(height, (hash, block.header.clone()));
            self.bodies.insert(hash);
            self.verified_block_tip = self.verified_block_tip.max(height);
        }

        fn has_body(&self, hash: block::Hash) -> bool {
            self.bodies.contains(&hash)
        }

        fn missing_bodies(&self, from: block::Height, limit: u32) -> Vec<block::Height> {
            let (tip, _) = self.best_header_tip();
            let start = self
                .verified_block_tip
                .next()
                .unwrap_or(self.verified_block_tip)
                .max(from);

            (start.0..=tip.0)
                .map(block::Height)
                .filter(|height| {
                    self.headers
                        .get(height)
                        .is_some_and(|(hash, _)| !self.bodies.contains(hash))
                })
                .take(limit as usize)
                .collect()
        }
    }

    #[derive(Clone, Debug)]
    struct E2eNodeView {
        peer_id: ZakuraPeerId,
        handle: HeaderSyncHandle,
        store: Arc<StdMutex<E2eHeaderStore>>,
        observed_gaps: Arc<Mutex<Vec<(block::Height, block::Height)>>>,
        disconnects: Arc<Mutex<Vec<(ZakuraPeerId, HeaderSyncMisbehavior)>>>,
        sent: Arc<Mutex<Vec<(ZakuraPeerId, HeaderSyncMessage)>>>,
        outbound_receivers: Arc<StdMutex<Vec<FramedRecv>>>,
    }

    #[derive(Debug)]
    struct E2eNode {
        view: E2eNodeView,
        actions: Option<mpsc::Receiver<HeaderSyncAction>>,
        task: JoinHandle<()>,
        shutdown: CancellationToken,
    }

    impl E2eNodeView {
        fn header_sync_session(&self, peer: ZakuraPeerId) -> HeaderSyncPeerSession {
            let (send, recv) = crate::zakura::framed_channel(32);
            self.outbound_receivers
                .lock()
                .expect("test outbound receiver mutex is not poisoned")
                .push(recv);
            HeaderSyncPeerSession::from_parts(peer, send, CancellationToken::new())
        }
    }

    #[derive(Debug)]
    struct HeaderSyncE2eCluster {
        nodes: Vec<E2eNode>,
        drivers: Vec<JoinHandle<()>>,
    }

    impl HeaderSyncE2eCluster {
        fn new() -> Self {
            Self {
                nodes: Vec::new(),
                drivers: Vec::new(),
            }
        }

        fn spawn_node(
            &mut self,
            seed: u8,
            network: Network,
            anchor: (block::Height, block::Hash),
            store: E2eHeaderStore,
            trace: ZakuraTrace,
        ) -> Result<usize, BoxError> {
            let store = Arc::new(StdMutex::new(store));
            let startup_store = store
                .lock()
                .map_err(|_| std::io::Error::other("test store mutex is poisoned"))?;
            let mut startup = HeaderSyncStartup::new(
                network,
                anchor,
                startup_store.frontiers(),
                Some(startup_store.best_header_tip()),
                ZakuraHeaderSyncConfig::default(),
                4 * 1024 * 1024,
            );
            drop(startup_store);
            startup.trace = trace;
            startup.range_state_actions_enabled = true;
            startup.inbound_new_block_acceptance_enabled = true;
            startup.status_refresh_interval = Duration::from_millis(200);
            startup.request_timeout = Duration::from_millis(500);
            let shutdown = CancellationToken::new();
            startup.shutdown = shutdown.clone();

            let (handle, actions, task) = spawn_header_sync_reactor(startup)?;
            let view = E2eNodeView {
                peer_id: e2e_peer(seed),
                handle,
                store,
                observed_gaps: Arc::new(Mutex::new(Vec::new())),
                disconnects: Arc::new(Mutex::new(Vec::new())),
                sent: Arc::new(Mutex::new(Vec::new())),
                outbound_receivers: Arc::new(StdMutex::new(Vec::new())),
            };
            self.nodes.push(E2eNode {
                view,
                actions: Some(actions),
                task,
                shutdown,
            });
            Ok(self.nodes.len() - 1)
        }

        fn start_drivers(&mut self) {
            let views: Vec<_> = self.nodes.iter().map(|node| node.view.clone()).collect();
            let peer_to_index: HashMap<_, _> = views
                .iter()
                .enumerate()
                .map(|(index, view)| (view.peer_id.clone(), index))
                .collect();

            for index in 0..self.nodes.len() {
                let Some(actions) = self.nodes[index].actions.take() else {
                    continue;
                };
                self.drivers.push(tokio::spawn(drive_e2e_node(
                    index,
                    actions,
                    views.clone(),
                    peer_to_index.clone(),
                )));
            }
        }

        async fn connect_all(&self) {
            for left in 0..self.nodes.len() {
                for right in 0..self.nodes.len() {
                    if left == right {
                        continue;
                    }
                    let peer = self.nodes[right].view.peer_id.clone();
                    let session = self.nodes[left].view.header_sync_session(peer);
                    self.nodes[left]
                        .view
                        .handle
                        .send(HeaderSyncEvent::PeerConnected(session))
                        .await
                        .unwrap();
                }
            }
        }

        async fn connect_peer(&self, node: usize, peer: ZakuraPeerId) {
            let session = self.nodes[node].view.header_sync_session(peer);
            self.nodes[node]
                .view
                .handle
                .send(HeaderSyncEvent::PeerConnected(session))
                .await
                .unwrap();
        }

        async fn inject(&self, node: usize, peer: ZakuraPeerId, msg: HeaderSyncMessage) {
            self.nodes[node]
                .view
                .handle
                .send(HeaderSyncEvent::WireMessage { peer, msg })
                .await
                .unwrap();
        }

        async fn commit_body(&self, node: usize, block: Arc<block::Block>) {
            self.nodes[node]
                .view
                .store
                .lock()
                .expect("test store mutex is not poisoned")
                .commit_body(block.clone());
            let height = block.coinbase_height().expect("test block has height");
            self.nodes[node]
                .view
                .handle
                .send(HeaderSyncEvent::FullBlockCommitted {
                    height,
                    hash: block.hash(),
                    header: block.header.clone(),
                })
                .await
                .unwrap();
        }

        async fn missing_bodies(&self, node: usize) -> Vec<block::Height> {
            self.nodes[node]
                .view
                .store
                .lock()
                .expect("test store mutex is not poisoned")
                .missing_bodies(block::Height(1), 100)
        }

        async fn finalized_height(&self, node: usize) -> block::Height {
            self.nodes[node]
                .view
                .store
                .lock()
                .expect("test store mutex is not poisoned")
                .finalized_height
        }

        async fn reject_next_commit(&self, node: usize, kind: HeaderSyncCommitFailureKind) {
            self.nodes[node]
                .view
                .store
                .lock()
                .expect("test store mutex is not poisoned")
                .reject_next_commit = Some(kind);
        }

        async fn wait_for_tip(&self, node: usize, height: block::Height) -> Result<(), BoxError> {
            let handle = self.nodes[node].view.handle.clone();
            await_until("header-sync e2e best tip", Duration::from_secs(5), || {
                handle.best_header_tip().0 >= height
            })
            .await
            .map_err(Into::into)
        }

        async fn wait_for_body(&self, node: usize, hash: block::Hash) -> Result<(), BoxError> {
            let store = self.nodes[node].view.store.clone();
            await_until(
                "header-sync e2e body commit",
                Duration::from_secs(5),
                || {
                    store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .has_body(hash)
                },
            )
            .await
            .map_err(Into::into)
        }

        async fn disconnect_reasons(&self, node: usize) -> Vec<HeaderSyncMisbehavior> {
            self.nodes[node]
                .view
                .disconnects
                .lock()
                .await
                .iter()
                .map(|(_, reason)| *reason)
                .collect()
        }

        async fn wait_for_disconnect_reason(
            &self,
            node: usize,
            reason: HeaderSyncMisbehavior,
        ) -> Result<(), BoxError> {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if self.disconnect_reasons(node).await.contains(&reason) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Box::new(WaitError::new(
                        "header-sync e2e disconnect reason",
                        Duration::from_secs(5),
                    )));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        async fn wait_for_get_headers(
            &self,
            node: usize,
            peer: &ZakuraPeerId,
            start_height: block::Height,
            count: u32,
        ) -> Result<(), BoxError> {
            let sent = self.nodes[node].view.sent.clone();
            let peer = peer.clone();
            await_until(
                format!(
                    "header-sync e2e outbound getheaders peer={peer:?} start={start_height:?} count={count}"
                ),
                Duration::from_secs(5),
                || {
                    sent.try_lock().is_ok_and(|sent| {
                        sent.iter().any(|(sent_peer, msg)| {
                            sent_peer == &peer
                                && matches!(
                                    msg,
                                    HeaderSyncMessage::GetHeaders {
                                        start_height: actual_start,
                                        count: actual_count,
                                    } if *actual_start == start_height && *actual_count == count
                                )
                        })
                    })
                },
            )
            .await
            .map_err(Into::into)
        }

        async fn observed_gaps(&self, node: usize) -> Vec<(block::Height, block::Height)> {
            self.nodes[node].view.observed_gaps.lock().await.clone()
        }

        async fn shutdown(&mut self) {
            for node in &self.nodes {
                node.shutdown.cancel();
                node.task.abort();
            }
            for driver in self.drivers.drain(..) {
                driver.abort();
            }
        }
    }

    async fn drive_e2e_node(
        index: usize,
        mut actions: mpsc::Receiver<HeaderSyncAction>,
        nodes: Vec<E2eNodeView>,
        peer_to_index: HashMap<ZakuraPeerId, usize>,
    ) {
        let local = nodes[index].clone();
        while let Some(action) = actions.recv().await {
            match action {
                HeaderSyncAction::SendMessage { peer, msg } => {
                    if let Some(target) = peer_to_index.get(&peer) {
                        let _ = nodes[*target]
                            .handle
                            .send(HeaderSyncEvent::WireMessage {
                                peer: local.peer_id.clone(),
                                msg,
                            })
                            .await;
                    } else {
                        local.sent.lock().await.push((peer, msg));
                    }
                }
                HeaderSyncAction::ForwardNewBlock { peer, block, .. } => {
                    if let Some(target) = peer_to_index.get(&peer) {
                        let _ = nodes[*target]
                            .handle
                            .send(HeaderSyncEvent::WireMessage {
                                peer: local.peer_id.clone(),
                                msg: HeaderSyncMessage::NewBlock(block),
                            })
                            .await;
                    } else {
                        local
                            .sent
                            .lock()
                            .await
                            .push((peer, HeaderSyncMessage::NewBlock(block)));
                    }
                }
                HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                    let headers = local
                        .store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .headers_by_range(start, count);
                    let returned_count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
                    if let Some(target) = peer_to_index.get(&peer) {
                        let _ = nodes[*target]
                            .handle
                            .send(HeaderSyncEvent::WireMessage {
                                peer: local.peer_id.clone(),
                                msg: headers_message(headers),
                            })
                            .await;
                        let _ = local
                            .handle
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count,
                            })
                            .await;
                    }
                }
                HeaderSyncAction::CommitHeaderRange {
                    peer,
                    anchor,
                    start_height,
                    headers,
                    finalized,
                    ..
                } => {
                    let count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
                    let result = local
                        .store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .commit_headers(anchor, start_height, headers, finalized);
                    match result {
                        Ok((tip_height, tip_hash)) => {
                            let frontiers = local
                                .store
                                .lock()
                                .expect("test store mutex is not poisoned")
                                .frontiers();
                            let _ = local
                                .handle
                                .send(HeaderSyncEvent::HeaderRangeCommitted {
                                    start_height,
                                    tip_height,
                                    tip_hash,
                                })
                                .await;
                            let _ = local
                                .handle
                                .send(HeaderSyncEvent::StateFrontiersChanged(frontiers))
                                .await;
                        }
                        Err(kind) => {
                            let _ = local
                                .handle
                                .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                    peer,
                                    start_height,
                                    count,
                                    kind,
                                })
                                .await;
                        }
                    }
                }
                HeaderSyncAction::QueryBestHeaderTip => {
                    let (tip_height, tip_hash) = local
                        .store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .best_header_tip();
                    let _ = local
                        .handle
                        .send(HeaderSyncEvent::HeaderRangeCommitted {
                            start_height: tip_height,
                            tip_height,
                            tip_hash,
                        })
                        .await;
                }
                HeaderSyncAction::QueryMissingBlockBodies { from, limit } => {
                    let heights = local
                        .store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .missing_bodies(from, limit);
                    if let (Some(first), Some(last)) =
                        (heights.first().copied(), heights.last().copied())
                    {
                        local.observed_gaps.lock().await.push((first, last));
                    }
                }
                HeaderSyncAction::BodyGaps { from, to } => {
                    local.observed_gaps.lock().await.push((from, to));
                }
                HeaderSyncAction::HeaderAdvanced { .. } => {}
                HeaderSyncAction::HeaderReanchored { .. } => {}
                HeaderSyncAction::NewBlockReceived {
                    peer,
                    height,
                    hash,
                    block,
                } => {
                    if local
                        .store
                        .lock()
                        .expect("test store mutex is not poisoned")
                        .has_body(hash)
                    {
                        let _ = local
                            .handle
                            .send(HeaderSyncEvent::NewBlockDuplicate { peer, height, hash })
                            .await;
                    } else {
                        local
                            .store
                            .lock()
                            .expect("test store mutex is not poisoned")
                            .commit_body(block.clone());
                        let _ = local
                            .handle
                            .send(HeaderSyncEvent::NewBlockAccepted {
                                peer,
                                height,
                                hash,
                                block,
                            })
                            .await;
                    }
                }
                HeaderSyncAction::Misbehavior { peer, reason } => {
                    local.disconnects.lock().await.push((peer.clone(), reason));
                    let _ = local
                        .handle
                        .send(HeaderSyncEvent::PeerDisconnected(peer))
                        .await;
                }
            }
        }
    }

    fn e2e_peer(byte: u8) -> ZakuraPeerId {
        ZakuraPeerId::new(vec![byte; 32]).expect("test peer id is within bounds")
    }

    fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
        Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
    }

    fn block_size(block: &block::Block) -> u32 {
        u32::try_from(
            block
                .zcash_serialize_to_vec()
                .expect("test block serializes")
                .len(),
        )
        .expect("test block size fits u32")
    }

    async fn drive_native_block_sync_actions(
        node: &ZakuraTestNode,
        blocks: Vec<Arc<block::Block>>,
        submitted: Arc<StdMutex<Vec<block::Height>>>,
    ) -> JoinHandle<()> {
        let endpoint = node.endpoint();
        let mut actions = node
            .take_block_sync_actions()
            .await
            .expect("block-sync action receiver is enabled");
        let by_height: BTreeMap<_, _> = blocks
            .into_iter()
            .map(|block| {
                (
                    block.coinbase_height().expect("test block has height"),
                    block,
                )
            })
            .collect();

        tokio::spawn(async move {
            while let Some(action) = actions.recv().await {
                let Some(handle) = endpoint.block_sync() else {
                    continue;
                };
                match action {
                    BlockSyncAction::QueryNeededBlocks {
                        verified_block_tip,
                        best_header_tip,
                    } => {
                        let metas = by_height
                            .range(
                                verified_block_tip.next().unwrap_or(verified_block_tip)
                                    ..=best_header_tip,
                            )
                            .map(|(height, block)| BlockSyncBlockMeta {
                                height: *height,
                                hash: block.hash(),
                                size: BlockSizeEstimate::Advertised(block_size(block)),
                            })
                            .collect();
                        let _ = handle.send(BlockSyncEvent::NeededBlocks(metas)).await;
                    }
                    BlockSyncAction::SubmitBlock { token, block } => {
                        let height = block.coinbase_height().expect("submitted block has height");
                        submitted
                            .lock()
                            .expect("submitted list mutex is not poisoned")
                            .push(height);
                        let _ = handle
                            .send(BlockSyncEvent::BlockApplyFinished {
                                token,
                                height,
                                hash: block.hash(),
                                result: BlockApplyResult::Committed,
                                local_frontier: Some(BlockSyncFrontiers {
                                    finalized_height: height,
                                    verified_block_tip: height,
                                    verified_block_hash: block.hash(),
                                }),
                            })
                            .await;
                    }
                    BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                        let _ = handle
                            .send(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    BlockSyncAction::Misbehavior { peer, .. } => {
                        let _ = endpoint.supervisor().disconnect_peer(&peer).await;
                    }
                    BlockSyncAction::SendMessage { .. } => {}
                }
            }
        })
    }

    fn block_bytes(height: u32) -> &'static [u8] {
        match height {
            1 => &BLOCK_MAINNET_1_BYTES,
            2 => &BLOCK_MAINNET_2_BYTES,
            3 => &BLOCK_MAINNET_3_BYTES,
            4 => &BLOCK_MAINNET_4_BYTES,
            5 => &BLOCK_MAINNET_5_BYTES,
            _ => panic!("missing test vector for height {height}"),
        }
    }

    fn mainnet_genesis_hash() -> block::Hash {
        mainnet_block(&BLOCK_MAINNET_GENESIS_BYTES).hash()
    }

    fn status_for_tip(
        height: u32,
        max_headers_per_response: u32,
        max_inflight_requests: u16,
    ) -> HeaderSyncMessage {
        let tip_hash = if height == 0 {
            mainnet_genesis_hash()
        } else {
            mainnet_block(block_bytes(height)).hash()
        };

        HeaderSyncMessage::Status(HeaderSyncStatus {
            tip_height: block::Height(height),
            tip_hash,
            anchor_height: block::Height(0),
            max_headers_per_response,
            max_inflight_requests,
        })
    }

    fn e2e_network(checkpoints: impl IntoIterator<Item = u32>) -> Network {
        let checkpoints = std::iter::once((block::Height(0), mainnet_genesis_hash()))
            .chain(checkpoints.into_iter().map(|height| {
                (
                    block::Height(height),
                    mainnet_block(block_bytes(height)).hash(),
                )
            }))
            .collect();

        TestnetParameters::build()
            .with_genesis_hash(mainnet_genesis_hash())
            .expect("mainnet genesis vector hash parses")
            .with_activation_heights(ConfiguredActivationHeights {
                before_overwinter: None,
                overwinter: Some(1),
                sapling: Some(1),
                blossom: Some(1),
                heartwood: Some(1),
                canopy: Some(1),
                nu5: None,
                nu6: None,
                nu6_1: None,
                nu6_2: None,
                nu7: None,
                #[cfg(zcash_unstable = "zfuture")]
                zfuture: None,
            })
            .expect("height-1 activation set is valid")
            .with_funding_streams(Vec::new())
            .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(checkpoints))
            .expect("e2e checkpoints use valid header hashes")
            .to_network()
            .expect("e2e network has enough checkpoint coverage")
    }

    fn e2e_network_with_checkpoint_hash(height: u32, hash: block::Hash) -> Network {
        let checkpoints = vec![
            (block::Height(0), mainnet_genesis_hash()),
            (block::Height(height), hash),
        ];

        TestnetParameters::build()
            .with_genesis_hash(mainnet_genesis_hash())
            .expect("mainnet genesis vector hash parses")
            .with_activation_heights(ConfiguredActivationHeights {
                before_overwinter: None,
                overwinter: Some(1),
                sapling: Some(1),
                blossom: Some(1),
                heartwood: Some(1),
                canopy: Some(1),
                nu5: None,
                nu6: None,
                nu6_1: None,
                nu6_2: None,
                nu7: None,
                #[cfg(zcash_unstable = "zfuture")]
                zfuture: None,
            })
            .expect("height-1 activation set is valid")
            .with_funding_streams(Vec::new())
            .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(checkpoints))
            .expect("e2e checkpoints use valid header hashes")
            .to_network()
            .expect("e2e network has enough checkpoint coverage")
    }

    fn checkpoint_network(checkpoint_height: u32) -> (Network, block::Hash) {
        let checkpoint_hash = mainnet_block(block_bytes(checkpoint_height)).hash();
        (e2e_network([checkpoint_height]), checkpoint_hash)
    }

    fn header_sync_test_builder(
        seed: u64,
        network: Network,
        trace: &mut TraceCapture,
    ) -> super::super::ZakuraTestNodeBuilder {
        let anchor = (block::Height(0), mainnet_genesis_hash());
        ZakuraTestNode::builder(seed)
            .tracer(trace.tracer_for_node(seed))
            .header_sync_driver(
                network,
                anchor,
                HeaderSyncFrontiers {
                    finalized_height: block::Height(0),
                    verified_block_tip: block::Height(0),
                    verified_block_hash: anchor.1,
                },
                Some(anchor),
            )
    }

    async fn drive_native_header_sync_actions(
        node: &ZakuraTestNode,
        disconnects: Arc<StdMutex<Vec<HeaderSyncMisbehavior>>>,
    ) -> JoinHandle<()> {
        let endpoint = node.endpoint();
        let supervisor = node.supervisor();
        let mut actions = node
            .take_header_sync_actions()
            .await
            .expect("header-sync action receiver is enabled");

        tokio::spawn(async move {
            while let Some(action) = actions.recv().await {
                match action {
                    HeaderSyncAction::SendMessage { .. }
                    | HeaderSyncAction::ForwardNewBlock { .. } => {}
                    HeaderSyncAction::Misbehavior { peer, reason } => {
                        disconnects
                            .lock()
                            .expect("disconnect list mutex is not poisoned")
                            .push(reason);
                        let _ = supervisor.disconnect_peer(&peer).await;
                    }
                    HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                        let Some(handle) = endpoint.header_sync() else {
                            continue;
                        };
                        let _ = handle
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    HeaderSyncAction::CommitHeaderRange {
                        peer,
                        start_height,
                        headers,
                        ..
                    } => {
                        let Some(handle) = endpoint.header_sync() else {
                            continue;
                        };
                        let count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
                        let _ = handle
                            .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                peer,
                                start_height,
                                count,
                                kind: HeaderSyncCommitFailureKind::Local,
                            })
                            .await;
                    }
                    HeaderSyncAction::NewBlockReceived {
                        peer, height, hash, ..
                    } => {
                        let Some(handle) = endpoint.header_sync() else {
                            continue;
                        };
                        let _ = handle
                            .send(HeaderSyncEvent::NewBlockDuplicate { peer, height, hash })
                            .await;
                    }
                    HeaderSyncAction::QueryBestHeaderTip
                    | HeaderSyncAction::QueryMissingBlockBodies { .. }
                    | HeaderSyncAction::BodyGaps { .. }
                    | HeaderSyncAction::HeaderAdvanced { .. }
                    | HeaderSyncAction::HeaderReanchored { .. } => {}
                }
            }
        })
    }

    async fn wait_for_native_disconnect(
        disconnects: Arc<StdMutex<Vec<HeaderSyncMisbehavior>>>,
        reason: HeaderSyncMisbehavior,
    ) -> Result<(), BoxError> {
        await_until(
            "native header-sync disconnect reason",
            Duration::from_secs(5),
            || {
                disconnects
                    .lock()
                    .expect("disconnect list mutex is not poisoned")
                    .contains(&reason)
            },
        )
        .await
        .map_err(Into::into)
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
    async fn native_stream6_oversize_frame_is_traceable_over_real_connection(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "native_stream6_oversize_frame_is_traceable_over_real_connection",
            false,
        )?;
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_traced_node(1, &mut capture).await?;
        let victim = cluster.node(victim_idx);
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 2, ZAKURA_CAP_BLOCK_SYNC).await?;
        let hostile_peer = hostile.id()?;
        let peer_set = victim.supervisor().subscribe();

        await_until("block-sync peer registered", Duration::from_secs(5), || {
            peer_set.borrow().contains(&hostile_peer)
        })
        .await?;
        assert!(
            MAX_BS_FRAME_BYTES < victim.limits().max_frame_bytes,
            "test payload must fit the negotiated connection cap but exceed stream-6's cap"
        );
        hostile
            .send_frame_header_with_declared_payload_len(
                ZAKURA_STREAM_BLOCK_SYNC,
                MAX_BS_FRAME_BYTES,
            )
            .await?;

        await_until("stream-6 oversize trace", Duration::from_secs(5), || {
            capture.reader().is_ok_and(|reader| {
                reader
                    .node("01")
                    .table("ratelimit")
                    .rows()
                    .iter()
                    .any(|row| {
                        row.get("event").and_then(serde_json::Value::as_str)
                            == Some("frame.oversize")
                            && row.get("stream_kind").and_then(serde_json::Value::as_str)
                                == Some("block_sync")
                    })
            })
        })
        .await?;
        await_until(
            "oversized stream-6 frame disconnects peer",
            Duration::from_secs(5),
            || !peer_set.borrow().contains(&hostile_peer),
        )
        .await?;

        hostile.shutdown().await;
        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn native_stream6_declared_payload_above_old_cap_is_not_raw_oversize(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "native_stream6_declared_payload_above_old_cap_is_not_raw_oversize",
            false,
        )?;
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_traced_node(1, &mut capture).await?;
        let victim = cluster.node(victim_idx);
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 2, ZAKURA_CAP_BLOCK_SYNC).await?;
        let hostile_peer = hostile.id()?;
        let peer_set = victim.supervisor().subscribe();

        await_until("block-sync peer registered", Duration::from_secs(5), || {
            peer_set.borrow().contains(&hostile_peer)
        })
        .await?;

        let old_max_bs_message_bytes =
            u32::try_from(block::MAX_BLOCK_BYTES).expect("max block bytes fits in u32") + 1;
        let regression_payload_len = old_max_bs_message_bytes + 1;
        assert!(
            regression_payload_len < MAX_BS_FRAME_BYTES,
            "test payload must exceed the old stream-6 cap but fit the new one"
        );

        hostile
            .send_frame_header_with_declared_payload_len(
                ZAKURA_STREAM_BLOCK_SYNC,
                regression_payload_len,
            )
            .await?;

        await_until(
            "incomplete stream-6 frame disconnects peer",
            Duration::from_secs(5),
            || !peer_set.borrow().contains(&hostile_peer),
        )
        .await?;

        let reader = capture.reader()?;
        let raw_oversize = reader
            .node("01")
            .table("ratelimit")
            .rows()
            .iter()
            .any(|row| {
                row.get("event").and_then(serde_json::Value::as_str) == Some("frame.oversize")
                    && row.get("stream_kind").and_then(serde_json::Value::as_str)
                        == Some("block_sync")
            });
        assert!(
            !raw_oversize,
            "payloads above the old stream-6 cap but below the new cap must reach \
             the stream payload reader instead of being dropped as raw frame.oversize"
        );

        hostile.shutdown().await;
        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn native_block_sync_getblocks_flushes_before_hostile_peer_sends_bodies(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "native_block_sync_getblocks_flushes_before_hostile_peer_sends_bodies",
            false,
        )?;
        let blocks = vec![
            mainnet_block(&BLOCK_MAINNET_1_BYTES),
            mainnet_block(&BLOCK_MAINNET_2_BYTES),
            mainnet_block(&BLOCK_MAINNET_3_BYTES),
        ];

        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = 16;
        limits.max_pending_handshakes = 8;
        limits.max_open_streams = 16;
        limits.max_inbound_queue_depth = 8;
        limits.message_rate_per_second = 64;
        limits.stream_open_rate_per_second = 64;

        let block_sync_config = ZakuraBlockSyncConfig {
            near_tip_body_download_pause_blocks: 0,
            max_blocks_per_response: 3,
            max_inflight_block_bytes: u64::MAX,
            request_timeout: Duration::from_secs(300),
            peer_limits: ServicePeerLimits {
                inbound_queue_depth: 1,
                outbound_queue_depth: 1,
                ..ServicePeerLimits::default()
            },
            ..ZakuraBlockSyncConfig::default()
        };

        let anchor = (block::Height(0), mainnet_genesis_hash());
        let mut cluster = ZakuraTestCluster::new();
        let victim = ZakuraTestNode::builder(60)
            .limits(limits)
            .tracer(capture.tracer_for_node(60))
            .header_sync_driver(
                e2e_network([3]),
                anchor,
                HeaderSyncFrontiers {
                    finalized_height: block::Height(0),
                    verified_block_tip: block::Height(0),
                    verified_block_hash: anchor.1,
                },
                Some((block::Height(3), blocks[2].hash())),
            )
            .block_sync_config(block_sync_config)
            .spawn()
            .await?;
        cluster.nodes.push(victim);
        let victim = cluster.node(0);
        assert!(
            victim.block_sync().is_some(),
            "block-sync handle should be enabled with the header-sync test driver"
        );

        let submitted = Arc::new(StdMutex::new(Vec::new()));
        let driver =
            drive_native_block_sync_actions(victim, blocks.clone(), submitted.clone()).await;
        let hostile =
            HostilePeer::connect_native_with_capabilities(victim, 61, ZAKURA_CAP_BLOCK_SYNC)
                .await?;
        let hostile_peer = hostile.id()?;
        let peer_set = victim.supervisor().subscribe();
        await_until("block-sync peer registered", Duration::from_secs(5), || {
            peer_set.borrow().contains(&hostile_peer)
        })
        .await?;

        hostile
            .send_raw_frame(
                ZAKURA_STREAM_BLOCK_SYNC,
                BlockSyncMessage::Status(BlockSyncStatus {
                    servable_low: block::Height(1),
                    servable_high: block::Height(3),
                    tip_hash: blocks[2].hash(),
                    max_blocks_per_response: 3,
                    max_inflight_requests: 1,
                    max_response_bytes: MAX_BS_RESPONSE_BYTES,
                })
                .encode_frame()?,
            )
            .await?;

        let (start_height, count) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = hostile.recv_ordered_frame(ZAKURA_STREAM_BLOCK_SYNC).await?;
                match BlockSyncMessage::decode_frame(frame)
                    .map_err(|error| -> BoxError { Box::new(error) })?
                {
                    BlockSyncMessage::GetBlocks {
                        start_height,
                        count,
                    } => return Ok::<_, BoxError>((start_height, count)),
                    BlockSyncMessage::Status(_) => {}
                    msg => {
                        return Err(format!("unexpected native block-sync message: {msg:?}").into())
                    }
                }
            }
        })
        .await
        .map_err(|_| -> BoxError {
            "timed out waiting for physical stream-6 GetBlocks frame".into()
        })??;

        assert_eq!(start_height, block::Height(1));
        assert_eq!(count, 3);
        assert!(
            submitted
                .lock()
                .expect("submitted list mutex is not poisoned")
                .is_empty(),
            "the test-side responder must not send bodies or trigger submissions before it has \
             physically read GetBlocks from stream 6"
        );

        let end_height = start_height
            .0
            .checked_add(count)
            .expect("test request height range fits u32");
        for height in start_height.0..end_height {
            let block = blocks
                .iter()
                .find(|block| block.coinbase_height() == Some(block::Height(height)))
                .expect("requested test block exists")
                .clone();
            hostile
                .send_raw_frame(
                    ZAKURA_STREAM_BLOCK_SYNC,
                    BlockSyncMessage::Block(block).encode_frame()?,
                )
                .await?;
        }
        hostile
            .send_raw_frame(
                ZAKURA_STREAM_BLOCK_SYNC,
                BlockSyncMessage::BlocksDone {
                    start_height,
                    returned: count,
                }
                .encode_frame()?,
            )
            .await?;

        let expected: Vec<_> = (start_height.0..end_height).map(block::Height).collect();
        await_until(
            "native block-sync submitted requested bodies",
            Duration::from_secs(5),
            || {
                let mut actual = submitted
                    .lock()
                    .expect("submitted list mutex is not poisoned")
                    .clone();
                actual.sort_unstable();
                actual.dedup();
                expected.iter().all(|height| actual.contains(height))
            },
        )
        .await?;

        await_until(
            "native block-sync submitted trace rows",
            Duration::from_secs(5),
            || {
                capture.reader().is_ok_and(|reader| {
                    let rows = reader.node("60").table("block_sync").rows();
                    expected.iter().all(|height| {
                        rows.iter().any(|row| {
                            row.get(bs_trace::EVENT).and_then(serde_json::Value::as_str)
                                == Some(bs_trace::BLOCK_BODY_SUBMITTED)
                                && row
                                    .get(bs_trace::HEIGHT)
                                    .and_then(serde_json::Value::as_u64)
                                    == Some(u64::from(height.0))
                        })
                    })
                })
            },
        )
        .await?;

        driver.abort();
        hostile.shutdown().await;
        cluster.shutdown().await;
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
    async fn recorder_transport_survives_malformed_header_sync_frame() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_node(5).await?;
        let victim = cluster.node(victim_idx);
        let recorder = victim.recorder();
        let hostile = HostilePeer::connect_native(victim, 6).await?;

        let before = b"before-header-sync-error".to_vec();
        let bad_header_sync_payload = vec![99];
        hostile
            .send_frame(ZAKURA_STREAM_HEADER_SYNC, bad_header_sync_payload.clone())
            .await?;
        hostile.send_frame(2, before.clone()).await?;
        await_until("pre-error gossip delivered", Duration::from_secs(5), || {
            recorder.contains_payload(2, &before)
        })
        .await?;

        let after = b"after-header-sync-error".to_vec();
        hostile.send_frame(2, after.clone()).await?;
        await_until(
            "post-header-sync gossip delivered",
            Duration::from_secs(5),
            || recorder.contains_payload(2, &after),
        )
        .await?;

        let delivered = recorder.drain();
        assert!(
            delivered
                .iter()
                .any(|m| m.stream_kind == 2 && m.frame.payload == before),
            "pre-error gossip frame must be delivered"
        );
        assert!(
            delivered
                .iter()
                .any(|m| m.stream_kind == ZAKURA_STREAM_HEADER_SYNC
                    && m.frame.payload == bad_header_sync_payload),
            "recorder nodes assert transport routing only; production header-sync owners decode stream-5 frames and reject malformed payloads, got {delivered:?}"
        );
        assert!(
            delivered.iter().any(|m| m.frame.payload == after),
            "generic transport must not close on header-sync payload decode, got {delivered:?}"
        );

        hostile.shutdown().await;
        cluster.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn unnegotiated_header_sync_stream_is_rejected_before_delivery() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut cluster = ZakuraTestCluster::new();
        let victim_idx = cluster.spawn_node(7).await?;
        let victim = cluster.node(victim_idx);
        let recorder = victim.recorder();

        let zero_cap_peer = HostilePeer::connect_native_with_capabilities(victim, 8, 0).await?;
        let rejected_payload = b"unnegotiated-header-sync".to_vec();
        zero_cap_peer
            .send_frame(ZAKURA_STREAM_HEADER_SYNC, rejected_payload.clone())
            .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !recorder.contains_payload(ZAKURA_STREAM_HEADER_SYNC, &rejected_payload),
            "stream 5 from a zero-capability peer must be rejected before delivery"
        );

        let header_cap_peer =
            HostilePeer::connect_native_with_capabilities(victim, 9, ZAKURA_CAP_HEADER_SYNC)
                .await?;
        let admitted_payload = b"negotiated-header-sync".to_vec();
        header_cap_peer
            .send_frame(ZAKURA_STREAM_HEADER_SYNC, admitted_payload.clone())
            .await?;
        await_until(
            "negotiated header-sync stream delivered",
            Duration::from_secs(5),
            || recorder.contains_payload(ZAKURA_STREAM_HEADER_SYNC, &admitted_payload),
        )
        .await?;

        zero_cap_peer.shutdown().await;
        header_cap_peer.shutdown().await;
        cluster.shutdown().await;
        Ok(())
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
                DiscoveryMessage::GetServices(_) => {}
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
        // Exceeding the per-kind message rate is traced at transport ingress
        // before the ordered stream is disconnected.
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
        // P2: a long-lived ordered stream spends the transport-owned per-kind
        // message-rate budget before frames reach the service. A peer that
        // floods past the budget is disconnected (we never drop a solicited
        // frame), so no more than ~one budget of frames is ever delivered.
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "persistent_ordered_stream_uses_message_budget",
            false,
        )?;

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

        let victim = ZakuraTestNode::builder(5)
            .limits(limits)
            .tracer(capture.tracer_for_node(5))
            .spawn()
            .await?;
        let recorder = victim.recorder();
        let hostile =
            HostilePeer::connect_native_with_capabilities(&victim, 6, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;

        let sent = message_budget * 8;
        for index in 0..sent {
            // Once the budget is exceeded the victim disconnects, so later
            // sends race the teardown and may error -- that is expected.
            if hostile
                .send_frame(2, format!("a-{index}").into_bytes())
                .await
                .is_err()
            {
                break;
            }
        }

        // Wait until rate limiting has clearly engaged (more frames sent than one
        // budget, so the bucket must have emptied at least once).
        await_until("rate limiting engaged", Duration::from_secs(5), || {
            capture.reader().is_ok_and(|reader| {
                reader
                    .node("05")
                    .table("ratelimit")
                    .rows()
                    .iter()
                    .any(|row| {
                        row.get("event").and_then(serde_json::Value::as_str)
                            == Some("message.throttled")
                            && row.get("stream_kind").and_then(serde_json::Value::as_str)
                                == Some("gossip")
                    })
            })
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
        assert!(capture.finish().await?.is_none());
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
        let mut removed = false;
        while !sink_exited || !source_exited || !removed {
            match wait_for_probe_event(&mut events_rx, "service task exit", |event| {
                matches!(
                    event,
                    TaskExitProbeEvent::SinkExited(peer)
                        | TaskExitProbeEvent::SourceExited(peer)
                        | TaskExitProbeEvent::Removed(peer)
                        if peer == &peer_id
                )
            })
            .await?
            {
                TaskExitProbeEvent::SinkExited(peer) if peer == peer_id => sink_exited = true,
                TaskExitProbeEvent::SourceExited(peer) if peer == peer_id => source_exited = true,
                TaskExitProbeEvent::Removed(peer) if peer == peer_id => removed = true,
                _ => {}
            }
        }

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_stream5_status_exchange_uses_handler_wire_path() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "native_stream5_status_exchange_uses_handler_wire_path",
            false,
        )?;
        let network = e2e_network([1]);
        let mut cluster = ZakuraTestCluster::new();
        let node1 = header_sync_test_builder(1, network.clone(), &mut capture)
            .spawn()
            .await?;
        let node2 = header_sync_test_builder(2, network, &mut capture)
            .spawn()
            .await?;
        let disconnects1 = Arc::new(StdMutex::new(Vec::new()));
        let disconnects2 = Arc::new(StdMutex::new(Vec::new()));
        let driver1 = drive_native_header_sync_actions(&node1, disconnects1.clone()).await;
        let driver2 = drive_native_header_sync_actions(&node2, disconnects2.clone()).await;
        cluster.nodes.push(node1);
        cluster.nodes.push(node2);

        cluster.connect_full_mesh(Duration::from_secs(5)).await?;
        cluster.await_all_connected(Duration::from_secs(5)).await?;
        await_until(
            "native stream-5 status received",
            Duration::from_secs(5),
            || {
                capture.reader().is_ok_and(|reader| {
                    reader
                        .node("02")
                        .table("header_sync")
                        .count(hs_trace::HEADER_STATUS_RECEIVED)
                        >= 1
                })
            },
        )
        .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        reader.node("01").table("stream").assert_row(
            "accepted",
            &[("stream_kind", TraceValue::Str("header_sync"))],
        );
        reader.node("02").table("stream").assert_row(
            "accepted",
            &[("stream_kind", TraceValue::Str("header_sync"))],
        );
        reader
            .node("01")
            .table("header_sync")
            .assert_event(hs_trace::HEADER_STATUS_SENT);
        reader
            .node("02")
            .table("header_sync")
            .assert_event(hs_trace::HEADER_STATUS_RECEIVED);
        assert!(disconnects1.lock().unwrap().is_empty());
        assert!(disconnects2.lock().unwrap().is_empty());

        cluster.shutdown().await;
        driver1.abort();
        driver2.abort();
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_stream5_hostile_bytes_disconnect_with_traceable_reasons() -> Result<(), BoxError>
    {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "native_stream5_hostile_bytes_disconnect_with_traceable_reasons",
            false,
        )?;
        let victim = header_sync_test_builder(11, e2e_network([1]), &mut capture)
            .spawn()
            .await?;
        let disconnects = Arc::new(StdMutex::new(Vec::new()));
        let driver = drive_native_header_sync_actions(&victim, disconnects.clone()).await;

        let malformed =
            HostilePeer::connect_native_with_capabilities(&victim, 12, ZAKURA_CAP_HEADER_SYNC)
                .await?;
        malformed
            .send_raw_frame(
                ZAKURA_STREAM_HEADER_SYNC,
                Frame {
                    message_type: 99,
                    flags: 0,
                    payload: Vec::new(),
                },
            )
            .await?;
        wait_for_native_disconnect(disconnects.clone(), HeaderSyncMisbehavior::MalformedMessage)
            .await?;
        malformed.shutdown().await;

        let unsolicited =
            HostilePeer::connect_native_with_capabilities(&victim, 13, ZAKURA_CAP_HEADER_SYNC)
                .await?;
        let unsolicited_headers =
            headers_message(vec![mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone()])
                .encode_frame()?;
        unsolicited
            .send_raw_frame(ZAKURA_STREAM_HEADER_SYNC, unsolicited_headers)
            .await?;
        wait_for_native_disconnect(
            disconnects.clone(),
            HeaderSyncMisbehavior::UnsolicitedHeaders,
        )
        .await?;
        unsolicited.shutdown().await;

        let oversized =
            HostilePeer::connect_native_with_capabilities(&victim, 14, ZAKURA_CAP_HEADER_SYNC)
                .await?;
        let oversized_peer = oversized.id()?;
        let peer_set = victim.supervisor().subscribe();
        await_until("oversized peer registered", Duration::from_secs(5), || {
            peer_set.borrow().contains(&oversized_peer)
        })
        .await?;
        oversized
            .oversize_frame_declared_len(ZAKURA_STREAM_HEADER_SYNC)
            .await?;
        await_until(
            "native stream-5 oversize trace",
            Duration::from_secs(5),
            || {
                capture.reader().is_ok_and(|reader| {
                    reader
                        .node("11")
                        .table("ratelimit")
                        .rows()
                        .iter()
                        .any(|row| {
                            row.get("event").and_then(serde_json::Value::as_str)
                                == Some("frame.oversize")
                                && row.get("stream_kind").and_then(serde_json::Value::as_str)
                                    == Some("header_sync")
                        })
                })
            },
        )
        .await?;
        await_until(
            "oversized persistent stream disconnects peer",
            Duration::from_secs(5),
            || !peer_set.borrow().contains(&oversized_peer),
        )
        .await?;
        oversized.shutdown().await;

        let truncated =
            HostilePeer::connect_native_with_capabilities(&victim, 15, ZAKURA_CAP_HEADER_SYNC)
                .await?;
        truncated
            .send_truncated_frame(ZAKURA_STREAM_HEADER_SYNC)
            .await?;
        truncated.shutdown().await;

        // Each misbehavior disconnect cancels the peer's connection token, so the
        // victim's connection handler exits via the cancellation arm and stamps
        // the `closed.neutral` row with the bounded `cancelled` reason.
        await_until(
            "victim traces closed.neutral with a bounded reason",
            Duration::from_secs(5),
            || {
                capture.reader().is_ok_and(|reader| {
                    reader.node("11").table("conn").rows().iter().any(|row| {
                        row.get("event").and_then(serde_json::Value::as_str)
                            == Some("closed.neutral")
                            && row.get("reason").and_then(serde_json::Value::as_str)
                                == Some("cancelled")
                    })
                })
            },
        )
        .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        let header_sync = reader.node("11").table("header_sync");
        header_sync.assert_header_disconnect("malformed_message");
        header_sync.assert_header_disconnect("unsolicited_headers");
        reader.node("11").table("stream").assert_row(
            "accepted",
            &[("stream_kind", TraceValue::Str("header_sync"))],
        );
        reader.node("11").table("ratelimit").assert_row(
            "frame.oversize",
            &[("stream_kind", TraceValue::Str("header_sync"))],
        );
        reader.node("11").table("conn").assert_row(
            "closed.neutral",
            &[("reason", TraceValue::Str("cancelled"))],
        );

        victim.shutdown().await;
        driver.abort();
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_status_trace_smoke_uses_real_emitted_rows() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_status_trace_smoke_uses_real_emitted_rows",
            false,
        )?;
        let mut cluster = HeaderSyncE2eCluster::new();
        let network = e2e_network([4]);
        let anchor = (block::Height(0), mainnet_genesis_hash());

        cluster.spawn_node(
            1,
            network.clone(),
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(1), "01"),
        )?;
        cluster.spawn_node(
            2,
            network,
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(2), "02"),
        )?;
        cluster.start_drivers();
        cluster.connect_all().await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        capture.flush().await;
        let reader = capture.reader()?;
        reader
            .node("01")
            .table("header_sync")
            .assert_event(hs_trace::HEADER_STATUS_SENT);
        reader
            .node("02")
            .table("header_sync")
            .assert_event(hs_trace::HEADER_STATUS_RECEIVED);

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_genesis_converges_and_body_gap_api_shrinks() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_genesis_converges_and_body_gap_api_shrinks",
            false,
        )?;
        let mut cluster = HeaderSyncE2eCluster::new();
        let network = e2e_network([4]);
        let anchor = (block::Height(0), mainnet_genesis_hash());
        let source = cluster.spawn_node(
            1,
            network.clone(),
            anchor,
            E2eHeaderStore::with_headers(4),
            ZakuraTrace::new(capture.tracer_for_node(1), "01"),
        )?;
        let empty = cluster.spawn_node(
            2,
            network,
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(2), "02"),
        )?;
        assert_eq!(source, 0);
        cluster.start_drivers();
        cluster.connect_all().await;
        cluster.wait_for_tip(empty, block::Height(4)).await?;

        let missing = cluster.missing_bodies(empty).await;
        assert_eq!(
            missing,
            vec![
                block::Height(1),
                block::Height(2),
                block::Height(3),
                block::Height(4)
            ]
        );
        assert!(
            !cluster.observed_gaps(empty).await.is_empty(),
            "body-gap watch/API surface must be observable without header-sync body commands"
        );

        for height in 1..=4 {
            cluster
                .commit_body(empty, mainnet_block(block_bytes(height)))
                .await;
        }
        await_until("missing body gap shrinks", Duration::from_secs(5), || {
            cluster
                .nodes
                .get(empty)
                .expect("node exists")
                .view
                .store
                .lock()
                .expect("test store mutex is not poisoned")
                .missing_bodies(block::Height(1), 100)
                .is_empty()
        })
        .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        let target_trace = reader.node("02").table("header_sync");
        target_trace.assert_event(hs_trace::HEADER_STATUS_RECEIVED);
        target_trace.assert_header_range_request(1, 4);
        target_trace.assert_header_range_response(1, 4);
        target_trace.assert_header_range_commit(1, 4);
        target_trace.assert_row(
            hs_trace::HEADER_MISSING_BODIES_REPORTED,
            &[
                (hs_trace::RANGE_START, TraceValue::U64(1)),
                (hs_trace::RANGE_COUNT, TraceValue::U64(4)),
            ],
        );

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_checkpoint_forward_then_backward_finalizes_backfill(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_checkpoint_forward_then_backward_finalizes_backfill",
            false,
        )?;
        let (network, checkpoint_hash) = checkpoint_network(3);
        let mut cluster = HeaderSyncE2eCluster::new();
        let source = cluster.spawn_node(
            1,
            network.clone(),
            (block::Height(0), mainnet_genesis_hash()),
            E2eHeaderStore::with_headers(4),
            ZakuraTrace::new(capture.tracer_for_node(1), "01"),
        )?;
        let checkpointed = cluster.spawn_node(
            2,
            network,
            (block::Height(3), checkpoint_hash),
            E2eHeaderStore::with_checkpoint_anchor(3),
            ZakuraTrace::new(capture.tracer_for_node(2), "02"),
        )?;
        assert_eq!(source, 0);
        cluster.start_drivers();
        cluster.connect_all().await;
        cluster.wait_for_tip(checkpointed, block::Height(4)).await?;
        await_until(
            "checkpoint backfill finalized",
            Duration::from_secs(5),
            || {
                cluster
                    .nodes
                    .get(checkpointed)
                    .expect("node exists")
                    .view
                    .store
                    .lock()
                    .expect("test store mutex is not poisoned")
                    .finalized_height
                    >= block::Height(3)
            },
        )
        .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        let target_trace = reader.node("02").table("header_sync");
        target_trace.assert_header_range_request(4, 1);
        target_trace.assert_header_range_request(1, 3);
        target_trace.assert_header_range_commit(1, 3);
        assert_eq!(
            cluster.finalized_height(checkpointed).await,
            block::Height(3)
        );

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_tip_flood_and_no_double_gossip_cover_both_orderings(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_tip_flood_and_no_double_gossip_cover_both_orderings",
            false,
        )?;
        let mut cluster = HeaderSyncE2eCluster::new();
        let network = e2e_network([]);
        let anchor = (block::Height(0), mainnet_genesis_hash());
        for seed in 1..=3 {
            cluster.spawn_node(
                seed,
                network.clone(),
                anchor,
                E2eHeaderStore::genesis_only(),
                ZakuraTrace::new(
                    capture.tracer_for_node(u64::from(seed)),
                    format!("{seed:02}"),
                ),
            )?;
        }
        cluster.start_drivers();
        cluster.connect_all().await;

        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let hash1 = block1.hash();
        cluster
            .inject(
                0,
                cluster.nodes[1].view.peer_id.clone(),
                HeaderSyncMessage::NewBlock(block1.clone()),
            )
            .await;
        cluster.wait_for_body(0, hash1).await?;
        cluster.wait_for_body(2, hash1).await?;

        cluster.commit_body(0, block1.clone()).await;
        cluster
            .inject(
                0,
                cluster.nodes[1].view.peer_id.clone(),
                HeaderSyncMessage::NewBlock(block1),
            )
            .await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            cluster.disconnect_reasons(0).await.is_empty(),
            "honest tip-flood and duplicate paths must not disconnect peers"
        );

        capture.flush().await;
        let reader = capture.reader()?;
        let node1_trace = reader.node("01").table("header_sync");
        node1_trace.assert_event(hs_trace::HEADER_NEW_BLOCK_RECEIVED);
        node1_trace.assert_event(hs_trace::HEADER_NEW_BLOCK_FORWARDED);
        node1_trace.assert_header_new_block_deduped("seen_cache");
        assert_eq!(
            node1_trace.count(hs_trace::HEADER_GET_HEADERS_SENT),
            0,
            "tip full-block flood must not use header advertise/body-pull"
        );

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_concurrent_duplicate_new_block_does_not_disconnect(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_concurrent_duplicate_new_block_does_not_disconnect",
            false,
        )?;
        let mut cluster = HeaderSyncE2eCluster::new();
        let network = e2e_network([]);
        let anchor = (block::Height(0), mainnet_genesis_hash());
        for seed in 1..=3 {
            cluster.spawn_node(
                seed,
                network.clone(),
                anchor,
                E2eHeaderStore::genesis_only(),
                ZakuraTrace::new(
                    capture.tracer_for_node(u64::from(seed)),
                    format!("{seed:02}"),
                ),
            )?;
        }
        cluster.connect_all().await;
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let hash1 = block1.hash();
        cluster
            .inject(
                0,
                cluster.nodes[1].view.peer_id.clone(),
                HeaderSyncMessage::NewBlock(block1.clone()),
            )
            .await;
        cluster
            .inject(
                0,
                cluster.nodes[2].view.peer_id.clone(),
                HeaderSyncMessage::NewBlock(block1),
            )
            .await;
        cluster.start_drivers();
        cluster.wait_for_body(0, hash1).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(cluster.disconnect_reasons(0).await.is_empty());

        capture.flush().await;
        capture
            .reader()?
            .node("01")
            .table("header_sync")
            .assert_header_new_block_deduped("pending_acceptance");

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_hostile_peer_disconnect_matrix_has_traceable_reasons(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_hostile_peer_disconnect_matrix_has_traceable_reasons",
            false,
        )?;
        let mut cluster = HeaderSyncE2eCluster::new();
        let network = e2e_network([]);
        let anchor = (block::Height(0), mainnet_genesis_hash());
        let victim = cluster.spawn_node(
            1,
            network.clone(),
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(1), "01"),
        )?;
        cluster.start_drivers();

        let unsolicited = e2e_peer(90);
        cluster.connect_peer(victim, unsolicited.clone()).await;
        cluster
            .inject(
                victim,
                unsolicited,
                headers_message(vec![mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone()]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::UnsolicitedHeaders)
            .await?;

        let out_of_range = e2e_peer(95);
        cluster.connect_peer(victim, out_of_range.clone()).await;
        cluster
            .inject(victim, out_of_range.clone(), status_for_tip(4, 4, 1))
            .await;
        cluster
            .wait_for_get_headers(victim, &out_of_range, block::Height(1), 4)
            .await?;
        cluster
            .inject(
                victim,
                out_of_range,
                headers_message(vec![mainnet_block(&BLOCK_MAINNET_2_BYTES).header.clone()]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::InvalidRange)
            .await?;

        let over_in_flight = e2e_peer(96);
        cluster.connect_peer(victim, over_in_flight.clone()).await;
        cluster
            .inject(victim, over_in_flight.clone(), status_for_tip(0, 4, 1))
            .await;
        for start in 1..=17 {
            cluster
                .inject(
                    victim,
                    over_in_flight.clone(),
                    HeaderSyncMessage::GetHeaders {
                        start_height: block::Height(start),
                        count: 1,
                    },
                )
                .await;
        }
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::GetHeadersSpam)
            .await?;

        let response_too_long = e2e_peer(97);
        cluster
            .connect_peer(victim, response_too_long.clone())
            .await;
        cluster
            .inject(victim, response_too_long.clone(), status_for_tip(4, 1, 1))
            .await;
        cluster
            .wait_for_get_headers(victim, &response_too_long, block::Height(1), 1)
            .await?;
        cluster
            .inject(
                victim,
                response_too_long,
                headers_message(vec![
                    mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_2_BYTES).header.clone(),
                ]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::ResponseTooLong)
            .await?;

        let bad_continuity_victim = cluster.spawn_node(
            3,
            network.clone(),
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(3), "03"),
        )?;
        cluster.start_drivers();
        let bad_continuity = e2e_peer(98);
        cluster
            .connect_peer(bad_continuity_victim, bad_continuity.clone())
            .await;
        cluster
            .inject(
                bad_continuity_victim,
                bad_continuity.clone(),
                status_for_tip(4, 4, 1),
            )
            .await;
        cluster
            .wait_for_get_headers(bad_continuity_victim, &bad_continuity, block::Height(1), 4)
            .await?;
        let mut non_contiguous = *mainnet_block(&BLOCK_MAINNET_2_BYTES).header;
        non_contiguous.previous_block_hash = block::Hash([7; 32]);
        cluster
            .inject(
                bad_continuity_victim,
                bad_continuity,
                headers_message(vec![
                    mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone(),
                    Arc::new(non_contiguous),
                ]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(bad_continuity_victim, HeaderSyncMisbehavior::InvalidRange)
            .await?;

        let bad_pow_victim = cluster.spawn_node(
            4,
            network.clone(),
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(4), "04"),
        )?;
        cluster.start_drivers();
        let bad_pow = e2e_peer(99);
        cluster.connect_peer(bad_pow_victim, bad_pow.clone()).await;
        cluster
            .inject(bad_pow_victim, bad_pow.clone(), status_for_tip(4, 4, 1))
            .await;
        cluster
            .wait_for_get_headers(bad_pow_victim, &bad_pow, block::Height(1), 4)
            .await?;
        let mut bad_pow_header = *mainnet_block(&BLOCK_MAINNET_1_BYTES).header;
        bad_pow_header.nonce = [1; 32].into();
        cluster
            .inject(
                bad_pow_victim,
                bad_pow,
                headers_message(vec![Arc::new(bad_pow_header)]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(bad_pow_victim, HeaderSyncMisbehavior::InvalidRange)
            .await?;

        let bad_daa_victim = cluster.spawn_node(
            5,
            network,
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(5), "05"),
        )?;
        cluster.start_drivers();
        let bad_daa = e2e_peer(100);
        cluster.connect_peer(bad_daa_victim, bad_daa.clone()).await;
        cluster
            .inject(bad_daa_victim, bad_daa.clone(), status_for_tip(4, 4, 1))
            .await;
        cluster
            .wait_for_get_headers(bad_daa_victim, &bad_daa, block::Height(1), 4)
            .await?;
        cluster
            .reject_next_commit(
                bad_daa_victim,
                HeaderSyncCommitFailureKind::InvalidPeerRange,
            )
            .await;
        cluster
            .inject(
                bad_daa_victim,
                bad_daa,
                headers_message(vec![
                    mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_2_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_3_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_4_BYTES).header.clone(),
                ]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(bad_daa_victim, HeaderSyncMisbehavior::InvalidRange)
            .await?;

        let bad_checkpoint_backfill = e2e_peer(101);
        let checkpoint_hash = mainnet_block(&BLOCK_MAINNET_1_BYTES).hash();
        let checkpoint_network = e2e_network_with_checkpoint_hash(3, checkpoint_hash);
        let checkpointed = cluster.spawn_node(
            6,
            checkpoint_network,
            (block::Height(3), checkpoint_hash),
            E2eHeaderStore::with_checkpoint_anchor(3),
            ZakuraTrace::new(capture.tracer_for_node(6), "06"),
        )?;
        cluster.start_drivers();
        cluster
            .connect_peer(checkpointed, bad_checkpoint_backfill.clone())
            .await;
        cluster
            .inject(
                checkpointed,
                bad_checkpoint_backfill.clone(),
                status_for_tip(3, 4, 1),
            )
            .await;
        cluster
            .wait_for_get_headers(checkpointed, &bad_checkpoint_backfill, block::Height(1), 3)
            .await?;
        cluster
            .inject(
                checkpointed,
                bad_checkpoint_backfill,
                headers_message(vec![
                    mainnet_block(&BLOCK_MAINNET_1_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_2_BYTES).header.clone(),
                    mainnet_block(&BLOCK_MAINNET_3_BYTES).header.clone(),
                ]),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(checkpointed, HeaderSyncMisbehavior::InvalidRange)
            .await?;

        let over_cap = e2e_peer(91);
        cluster.connect_peer(victim, over_cap.clone()).await;
        cluster
            .inject(
                victim,
                over_cap.clone(),
                HeaderSyncMessage::Status(Default::default()),
            )
            .await;
        cluster
            .inject(
                victim,
                over_cap,
                HeaderSyncMessage::GetHeaders {
                    start_height: block::Height(1),
                    count: 4_001,
                },
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::GetHeadersTooLong)
            .await?;

        let status_spam = e2e_peer(92);
        cluster.connect_peer(victim, status_spam.clone()).await;
        cluster
            .inject(
                victim,
                status_spam.clone(),
                HeaderSyncMessage::Status(Default::default()),
            )
            .await;
        cluster
            .inject(
                victim,
                status_spam,
                HeaderSyncMessage::Status(Default::default()),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::StatusSpam)
            .await?;

        let new_block_spam = e2e_peer(93);
        cluster.connect_peer(victim, new_block_spam.clone()).await;
        cluster
            .inject(
                victim,
                new_block_spam.clone(),
                HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_1_BYTES)),
            )
            .await;
        cluster
            .inject(
                victim,
                new_block_spam,
                HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_2_BYTES)),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::NewBlockSpam)
            .await?;

        let invalid_block = e2e_peer(94);
        cluster.connect_peer(victim, invalid_block.clone()).await;
        let mut bad_block = (*mainnet_block(&BLOCK_MAINNET_1_BYTES)).clone();
        bad_block.transactions.clear();
        cluster
            .inject(
                victim,
                invalid_block,
                HeaderSyncMessage::NewBlock(Arc::new(bad_block)),
            )
            .await;
        cluster
            .wait_for_disconnect_reason(victim, HeaderSyncMisbehavior::MalformedMessage)
            .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        let trace = reader.node("01").table("header_sync");
        trace.assert_header_disconnect("unsolicited_headers");
        trace.assert_header_disconnect("invalid_range");
        trace.assert_header_disconnect("get_headers_spam");
        trace.assert_header_disconnect("response_too_long");
        trace.assert_header_disconnect("get_headers_too_long");
        trace.assert_header_disconnect("status_spam");
        trace.assert_header_disconnect("new_block_spam");
        trace.assert_header_disconnect("malformed_message");
        for node in ["03", "04", "05", "06"] {
            reader
                .node(node)
                .table("header_sync")
                .assert_header_disconnect("invalid_range");
        }

        cluster.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn header_sync_e2e_restart_reloads_durable_tip_and_rebuilds_scheduler(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let mut capture = TraceCapture::for_test_with_keep_override(
            "header_sync_e2e_restart_reloads_durable_tip_and_rebuilds_scheduler",
            false,
        )?;
        let network = e2e_network([4]);
        let anchor = (block::Height(0), mainnet_genesis_hash());

        let mut first = HeaderSyncE2eCluster::new();
        let seeded = first.spawn_node(
            1,
            network.clone(),
            anchor,
            E2eHeaderStore::with_headers(4),
            ZakuraTrace::new(capture.tracer_for_node(1), "01"),
        )?;
        let syncing = first.spawn_node(
            2,
            network.clone(),
            anchor,
            E2eHeaderStore::genesis_only(),
            ZakuraTrace::new(capture.tracer_for_node(2), "02"),
        )?;
        first.start_drivers();
        first.connect_all().await;
        first.wait_for_tip(syncing, block::Height(4)).await?;
        let durable_store = first.nodes[syncing].view.store.clone();
        first.shutdown().await;

        let restart_store = {
            let store = durable_store
                .lock()
                .expect("test store mutex is not poisoned");
            E2eHeaderStore {
                headers: store.headers.clone(),
                bodies: store.bodies.clone(),
                finalized_height: store.finalized_height,
                verified_block_tip: store.verified_block_tip,
                reject_next_commit: None,
            }
        };

        let mut restarted = HeaderSyncE2eCluster::new();
        restarted.spawn_node(
            1,
            network.clone(),
            anchor,
            E2eHeaderStore::with_headers(5),
            ZakuraTrace::new(capture.tracer_for_node(3), "03"),
        )?;
        let restarted_idx = restarted.spawn_node(
            2,
            network,
            anchor,
            restart_store,
            ZakuraTrace::new(capture.tracer_for_node(4), "04"),
        )?;
        restarted.start_drivers();
        restarted.connect_all().await;
        restarted
            .wait_for_tip(restarted_idx, block::Height(5))
            .await?;

        capture.flush().await;
        let reader = capture.reader()?;
        reader
            .node("04")
            .table("header_sync")
            .assert_header_range_request(5, 1);
        assert_eq!(
            restarted.nodes[restarted_idx]
                .view
                .handle
                .best_header_tip()
                .0,
            block::Height(5)
        );
        assert_eq!(seeded, 0);

        restarted.shutdown().await;
        assert!(capture.finish().await?.is_none());
        Ok(())
    }
}
