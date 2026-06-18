use super::{config::*, events::*, reorder::*, scheduler::*, state::*, wire::*, *};
use crate::zakura::{
    FrontierChange, FrontierUpdate, ServiceAdmissionDecision, ServicePeerDirection,
    ServicePeerSnapshot, ZakuraBlockSyncCandidateState,
};
use iroh::NodeId;

const SOFT_MISBEHAVIOR_DISCONNECT_THRESHOLD: u32 = 3;

/// Upper bound on how long the reactor will wait to enqueue a data-plane action
/// before abandoning it. The bounded `actions` channel is normally drained by
/// the action driver almost immediately; this deadline only trips when that
/// driver is genuinely stalled on backend/verifier work, and it keeps a stalled
/// driver from wedging the reactor's control plane — peer-lifecycle draining,
/// request timeouts, and above all misbehavior disconnects.
const ACTION_SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OutstandingRangeDisposition {
    Satisfied,
    RetryOriginal,
    RetryMissing,
}

/// Spawn a block-sync reactor and return its handle plus action stream.
pub fn spawn_block_sync_reactor(
    startup: BlockSyncStartup,
) -> (
    BlockSyncHandle,
    mpsc::Receiver<BlockSyncAction>,
    JoinHandle<()>,
) {
    debug_assert!(
        !startup.state_queries_enabled
            || (startup.header_tip.is_some() ^ startup.frontier_updates.is_some()),
        "state-backed block sync must have exactly one frontier source",
    );

    let state = BlockSyncState::new(&startup);
    let (events_tx, events_rx) =
        mpsc::channel(startup.config.peer_limits.inbound_queue_depth.max(1));
    let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
    let (actions_tx, actions_rx) = mpsc::channel(128);
    let (peers_tx, peers_rx) = watch::channel(state.peer_snapshot(startup.config.peer_limits));
    let (status_tx, status_rx) = watch::channel(state.last_advertised_status);
    let (candidates_tx, candidates_rx) = watch::channel(ZakuraBlockSyncCandidateState::default());

    let handle = BlockSyncHandle {
        events: events_tx,
        lifecycle: lifecycle_tx,
        peers: peers_rx,
        status: status_rx,
        candidates: candidates_rx,
    };
    let reactor = BlockSyncReactor {
        startup,
        state,
        events: events_rx,
        lifecycle: lifecycle_rx,
        actions: actions_tx,
        peers: peers_tx,
        status: status_tx,
        candidates: candidates_tx,
    };
    let task = tokio::spawn(reactor.run());

    (handle, actions_rx, task)
}

#[derive(Debug)]
pub(super) struct BlockSyncReactor {
    startup: BlockSyncStartup,
    state: BlockSyncState,
    events: mpsc::Receiver<BlockSyncEvent>,
    lifecycle: mpsc::UnboundedReceiver<BlockSyncEvent>,
    actions: mpsc::Sender<BlockSyncAction>,
    peers: watch::Sender<ServicePeerSnapshot>,
    status: watch::Sender<BlockSyncStatus>,
    candidates: watch::Sender<ZakuraBlockSyncCandidateState>,
}

impl BlockSyncReactor {
    async fn run(mut self) {
        let mut header_tip = self.startup.header_tip.clone();
        let mut header_tip_open = header_tip.is_some();
        let mut frontier_updates = self.startup.frontier_updates.clone();
        let mut frontier_updates_open = frontier_updates.is_some();
        let mut ticks = time::interval(self.startup.config.request_timeout);
        let mut status_ticks = time::interval(
            self.startup
                .config
                .status_refresh_interval
                .max(Duration::from_millis(1)),
        );

        if !self.query_needed_blocks().await {
            self.pause_new_body_downloads();
        }
        self.release_caught_up_block_sync_peers();
        self.publish_metrics();
        self.trace_sync_state();
        loop {
            tokio::select! {
                _ = self.startup.shutdown.cancelled() => break,
                event = self.lifecycle.recv() => {
                    let Some(event) = event else { break };
                    self.handle_event(event).await;
                }
                event = self.events.recv() => {
                    let Some(event) = event else { break };
                    self.handle_event(event).await;
                }
                changed = async {
                    match header_tip.as_mut() {
                        Some(header_tip) => header_tip.changed().await,
                        None => std::future::pending().await,
                    }
                }, if header_tip_open => {
                    match changed {
                        Ok(()) => {
                            let header_tip = header_tip
                                .as_mut()
                                .expect("header tip receiver exists while header_tip_open is true");
                            let (height, hash) = *header_tip.borrow_and_update();
                            self.handle_header_tip_changed(height, hash).await;
                            self.publish_metrics();
                        }
                        Err(_) => header_tip_open = false,
                    }
                }
                changed = async {
                    match frontier_updates.as_mut() {
                        Some(frontier_updates) => frontier_updates.changed().await,
                        None => std::future::pending().await,
                    }
                }, if frontier_updates_open => {
                    match changed {
                        Ok(()) => {
                            let frontier_updates = frontier_updates
                                .as_mut()
                                .expect("frontier update receiver exists while frontier_updates_open is true");
                            let update = *frontier_updates.borrow_and_update();
                            self.handle_frontier_update(update).await;
                            self.publish_metrics();
                        }
                        Err(_) => frontier_updates_open = false,
                    }
                }
                _ = ticks.tick() => {
                    self.handle_timeouts().await;
                    self.publish_metrics();
                    self.trace_sync_state();
                }
                _ = status_ticks.tick() => self.flush_status_refresh().await,
            }
        }
    }

    async fn handle_event(&mut self, event: BlockSyncEvent) {
        self.trace_event_received(&event);
        match event {
            BlockSyncEvent::PeerConnected(session) => self.handle_peer_connected(session).await,
            BlockSyncEvent::PeerDisconnected(peer) => self.handle_peer_disconnected(peer),
            BlockSyncEvent::WireMessage { peer, msg } => self.handle_wire_message(peer, msg).await,
            BlockSyncEvent::WireDecodeFailed { peer, error } => {
                self.handle_wire_decode_failed(peer, error).await
            }
            BlockSyncEvent::HeaderTipChanged { height, hash } => {
                self.handle_header_tip_changed(height, hash).await
            }
            BlockSyncEvent::StateFrontiersChanged(frontiers) => {
                self.handle_state_frontiers_changed(frontiers).await
            }
            BlockSyncEvent::ChainTipGrow(frontiers) => {
                self.handle_state_frontiers_changed(frontiers).await
            }
            BlockSyncEvent::ChainTipReset(frontiers) => {
                self.handle_chain_tip_reset(frontiers, true).await
            }
            BlockSyncEvent::NeededBlocks(blocks) => {
                self.handle_needed_blocks(blocks).await;
            }
            BlockSyncEvent::BlockApplyFinished {
                token,
                height,
                hash,
                result,
                local_frontier,
            } => {
                self.handle_block_apply_finished(token, height, hash, result, local_frontier)
                    .await
            }
            BlockSyncEvent::BlockRangeResponseReady {
                peer,
                start_height,
                requested_count,
                blocks,
            } => {
                self.handle_block_range_response_ready(peer, start_height, requested_count, blocks)
                    .await;
            }
            BlockSyncEvent::BlockRangeResponseFinished {
                peer,
                start_height,
                requested_count,
                returned_count,
            } => {
                self.handle_block_range_response_finished(
                    peer,
                    start_height,
                    requested_count,
                    returned_count,
                )
                .await;
            }
        }
        self.publish_metrics();
    }

    fn admission_decision_for(
        &self,
        peer: &ZakuraPeerId,
        direction: ServicePeerDirection,
    ) -> ServiceAdmissionDecision {
        if self.state.peers.contains_key(peer) {
            return ServiceAdmissionDecision::Admit;
        }

        let limits = self.startup.config.peer_limits;
        let admitted = self.admitted_count(direction);
        let cap = match direction {
            ServicePeerDirection::Inbound => limits.max_inbound_peers,
            ServicePeerDirection::Outbound => limits.max_outbound_peers,
        };

        if admitted >= cap {
            ServiceAdmissionDecision::RejectFull
        } else {
            ServiceAdmissionDecision::Admit
        }
    }

    fn admitted_count(&self, direction: ServicePeerDirection) -> usize {
        self.state
            .peers
            .values()
            .filter(|peer| peer.direction == direction)
            .count()
    }

    fn publish_peer_snapshot(&self) {
        let _ = self
            .peers
            .send(self.state.peer_snapshot(self.startup.config.peer_limits));
    }

    fn publish_candidate_state(&self) {
        let has_body_gaps = !self.state.needed_heights.is_empty();
        let mut admitted_node_ids: Vec<_> = self
            .state
            .peers
            .iter()
            .filter_map(|(peer_id, peer)| {
                if has_body_gaps && !peer.can_serve_any(&self.state.needed_heights) {
                    return None;
                }

                node_id_from_block_peer_id(peer_id)
            })
            .collect();
        admitted_node_ids.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        admitted_node_ids.dedup();

        let _ = self.candidates.send(ZakuraBlockSyncCandidateState {
            missing_block_bodies: self.state.needed_heights.clone(),
            admitted_node_ids,
        });
    }

    async fn handle_peer_connected(&mut self, session: BlockSyncPeerSession) {
        let peer = session.peer_id().clone();
        let direction = session.direction();
        self.state.disconnected_peers.remove(&peer);
        let decision = self.admission_decision_for(&peer, direction);
        if decision != ServiceAdmissionDecision::Admit {
            self.state.parked_peers.insert(peer);
            session.cancel_token().cancel();
            self.publish_peer_snapshot();
            self.publish_candidate_state();
            return;
        }

        self.state.parked_peers.remove(&peer);
        self.state
            .peers
            .entry(peer.clone())
            .and_modify(|peer_state| {
                peer_state.session = session.clone();
                peer_state.direction = direction;
            })
            .or_insert_with(|| PeerBlockState::new(session, &self.startup.config));
        if let Some(peer_state) = self.state.peers.get_mut(&peer) {
            peer_state.unsolicited.mark_taken(Instant::now());
        }
        self.trace_peer_connected(&peer, direction);
        self.publish_peer_snapshot();
        self.publish_candidate_state();
        self.send_status(&peer, "peer_connected").await;
        self.schedule().await;
    }

    fn handle_peer_disconnected(&mut self, peer: ZakuraPeerId) {
        if let Some(peer_state) = self.state.peers.remove(&peer) {
            self.trace_peer_disconnected(&peer, peer_state.received_status);
            for outstanding in peer_state.outstanding.into_iter().rev() {
                self.finish_detached_outstanding(
                    outstanding,
                    OutstandingRangeDisposition::RetryMissing,
                );
            }
            self.state.disconnected_peers.insert(peer.clone());
        }
        self.state.parked_peers.remove(&peer);
        self.state.schedule.forget_peer(&peer);
        self.publish_peer_snapshot();
        self.publish_candidate_state();
    }

    async fn handle_header_tip_changed(&mut self, height: block::Height, hash: block::Hash) {
        self.state.best_header_tip = height;
        self.state.best_header_hash = hash;
        if !self.query_needed_blocks().await {
            self.pause_new_body_downloads();
        }
        self.release_caught_up_block_sync_peers();
    }

    async fn handle_frontier_update(&mut self, update: FrontierUpdate) {
        let frontier = update.frontier;
        let state_frontiers = BlockSyncFrontiers {
            finalized_height: frontier.finalized.height,
            verified_block_tip: frontier.verified_body.height,
            verified_block_hash: frontier.verified_body.hash,
        };
        match update.change {
            FrontierChange::Snapshot => {
                self.handle_header_tip_changed(
                    frontier.best_header.height,
                    frontier.best_header.hash,
                )
                .await;
                self.handle_state_frontiers_changed(state_frontiers).await;
            }
            FrontierChange::HeaderAdvanced => {
                self.handle_header_tip_changed(
                    frontier.best_header.height,
                    frontier.best_header.hash,
                )
                .await;
                if frontier.verified_body.height > self.state.verified_block_tip {
                    self.handle_state_frontiers_changed(state_frontiers).await;
                }
            }
            FrontierChange::HeaderReanchored => {
                self.state.best_header_tip = frontier.best_header.height;
                self.state.best_header_hash = frontier.best_header.hash;
                self.handle_chain_tip_reset(state_frontiers, false).await;
            }
            FrontierChange::VerifiedGrow => {
                self.handle_state_frontiers_changed(state_frontiers).await;
                if frontier.best_header.height > self.state.best_header_tip {
                    self.handle_header_tip_changed(
                        frontier.best_header.height,
                        frontier.best_header.hash,
                    )
                    .await;
                }
            }
            FrontierChange::VerifiedReset => {
                self.handle_chain_tip_reset(state_frontiers, true).await;
                if frontier.best_header.height > self.state.best_header_tip {
                    self.handle_header_tip_changed(
                        frontier.best_header.height,
                        frontier.best_header.hash,
                    )
                    .await;
                }
            }
        }
    }

    async fn handle_state_frontiers_changed(&mut self, frontiers: BlockSyncFrontiers) {
        if let Some(old_serving_tip) = self.apply_state_frontiers_changed(frontiers, true).await {
            self.finish_frontier_update(old_serving_tip).await;
        }
    }

    async fn apply_state_frontiers_changed(
        &mut self,
        frontiers: BlockSyncFrontiers,
        release_applied: bool,
    ) -> Option<(block::Height, block::Hash)> {
        self.state.finalized_height = self.state.finalized_height.max(frontiers.finalized_height);
        if frontiers.verified_block_tip < self.state.verified_block_tip {
            tracing::debug!(
                current = ?self.state.verified_block_tip,
                stale = ?frontiers.verified_block_tip,
                "ignoring stale Zakura block-sync frontier update"
            );
            return None;
        }

        let old_serving_tip = (self.state.servable_high, self.state.servable_hash);
        self.state.servable_high = frontiers.verified_block_tip;
        self.state.servable_hash = frontiers.verified_block_hash;
        self.state.verified_block_hash = frontiers.verified_block_hash;
        self.state.body_download_floor = self
            .state
            .body_download_floor
            .max(frontiers.verified_block_tip);
        if frontiers.verified_block_tip != self.state.verified_block_tip {
            self.state
                .reorder
                .drop_through(frontiers.verified_block_tip, &mut self.state.budget);
            self.state
                .schedule
                .drop_through(frontiers.verified_block_tip);
            if release_applied {
                self.release_applied_blocks_through(frontiers.verified_block_tip);
            }
            self.drop_outstanding_through(frontiers.verified_block_tip);
            self.state.verified_block_tip = frontiers.verified_block_tip;
            self.trace_frontiers_changed(frontiers.verified_block_tip);
            self.release_contiguous_blocks().await;
        }
        Some(old_serving_tip)
    }

    async fn finish_frontier_update(&mut self, old_serving_tip: (block::Height, block::Hash)) {
        self.queue_status_refresh_if_changed(old_serving_tip);
        self.flush_status_refresh().await;
        if !self.query_needed_blocks().await {
            self.pause_new_body_downloads();
        }
        self.release_caught_up_block_sync_peers();
    }

    async fn handle_chain_tip_reset(
        &mut self,
        frontiers: BlockSyncFrontiers,
        preserve_active_successors: bool,
    ) {
        let reset_tip_matches_local_work = !self.reset_tip_conflicts_with_local_work(
            &frontiers,
            frontiers.verified_block_tip <= self.state.body_download_floor,
        );

        // State can report a forward `Reset` while checkpoint commits advance
        // under already-submitted or still-downloading successor bodies. Treat
        // that as verified growth once it is inside our submitted/downloaded
        // floor, or when we already have successor work in flight. Keep fork
        // resets destructive when they are not anchored by active successor
        // work.
        if frontiers.verified_block_tip > self.state.verified_block_tip
            && (frontiers.verified_block_tip <= self.state.body_download_floor
                || self.has_active_successor_after(frontiers.verified_block_tip))
            && reset_tip_matches_local_work
        {
            self.handle_state_frontiers_changed(frontiers).await;
            return;
        }

        metrics::counter!("sync.block.reorg.reset").increment(1);
        self.trace_chain_tip_reset(frontiers.verified_block_tip);

        // A `Reset` can also be a stale or coalesced state update for a tip
        // already inside our contiguous submitted/downloaded body floor. Do not
        // destructively clear successor bodies in that case: a stale reset
        // snapshot can otherwise erase `applying`/covered state and re-request
        // the same bodies while their first apply is still in flight.
        if preserve_active_successors
            && frontiers.verified_block_tip < self.state.body_download_floor
            && reset_tip_matches_local_work
            && self.has_active_successor_after(frontiers.verified_block_tip)
            && self.active_successor_links_to_anchor(
                frontiers.verified_block_tip,
                frontiers.verified_block_hash,
            )
        {
            self.handle_state_frontiers_changed(frontiers).await;
            return;
        }

        let remember_released_applies = frontiers.verified_block_tip > frontiers.finalized_height
            && frontiers.verified_block_tip <= self.state.body_download_floor;

        self.state.finalized_height = frontiers.finalized_height;
        self.state.verified_block_tip = frontiers.verified_block_tip;
        self.state.verified_block_hash = frontiers.verified_block_hash;
        self.state.body_download_floor = frontiers.verified_block_tip;
        let old_serving_tip = (self.state.servable_high, self.state.servable_hash);
        self.state.servable_high = frontiers.verified_block_tip;
        self.state.servable_hash = frontiers.verified_block_hash;

        self.state.reorder.clear(&mut self.state.budget);
        self.release_all_applying_blocks_for_reset(remember_released_applies);
        self.state.schedule.clear_covered_from(block::Height::MIN);
        self.drop_ranges_not_in_needed(&HashMap::new());
        self.state.schedule.retain_matching_needed(&HashMap::new());

        self.queue_status_refresh_if_changed(old_serving_tip);
        self.flush_status_refresh().await;
        if !self.query_needed_blocks().await {
            self.pause_new_body_downloads();
        }
        self.release_caught_up_block_sync_peers();
    }

    async fn handle_needed_blocks(&mut self, blocks: Vec<BlockSyncBlockMeta>) {
        // The state reports every header-known, body-missing height above the
        // download floor, but it has no visibility into our in-memory buffers.
        // Heights already at or below the body download floor, held in the
        // reorder buffer (received, waiting for a lower gap to fill), or in
        // `applying` (submitted, awaiting commit) must not be scheduled again:
        // `refresh_needed` builds one maximal contiguous range and `ensure`
        // rejects any range overlapping a queued/assigned one, so a held run
        // sitting above an open gap would otherwise block the gap below it from
        // ever being queued, freezing `body_download_floor` and re-requesting
        // already-held blocks forever. Only schedule heights we do not already
        // hold in memory and have not already submitted contiguously.
        let blocks: Vec<_> = blocks
            .into_iter()
            .filter(|block| {
                block.height > self.state.body_download_floor
                    && !self.state.reorder.contains(block.height)
                    && !self.state.applying.contains_key(&block.height)
                    && !self.has_submitted_apply(block.height, block.hash)
                    && !self.has_outstanding_request(block.height, block.hash)
            })
            .collect();

        self.state.needed_heights = blocks.iter().map(|block| block.height).collect();
        self.state.needed_heights.sort_unstable();
        self.state.needed_heights.dedup();
        self.publish_candidate_state();

        let needed = blocks
            .into_iter()
            .map(|block| NeededBlock {
                height: block.height,
                hash: block.hash,
                size: block.size,
            })
            .collect::<Vec<_>>();
        let needed_hashes = needed
            .iter()
            .map(|block| (block.height, block.hash))
            .collect::<HashMap<_, _>>();
        // State queries are snapshots taken while peer responses are still in
        // flight. A newer snapshot can omit heights from an active request
        // because those bodies are already buffered, applying, verified, or the
        // query simply raced with the response. Keep active ranges correlated
        // unless state explicitly reports a different hash for one of their
        // heights; otherwise valid late bodies become `UnsolicitedBlock` and
        // cause avoidable peer churn.
        let retention_hashes = self.needed_hashes_with_active_outstanding(&needed_hashes);
        self.drop_ranges_not_in_needed(&retention_hashes);
        self.state
            .schedule
            .retain_matching_needed(&retention_hashes);
        self.state.schedule.refresh_needed(needed);
        if self.should_pause_new_body_downloads() {
            self.pause_new_body_downloads();
            self.release_caught_up_block_sync_peers();
            return;
        }
        self.schedule().await;
    }

    fn clear_needed_heights(&mut self) {
        if self.state.needed_heights.is_empty() {
            return;
        }
        self.state.needed_heights.clear();
        self.publish_candidate_state();
    }

    fn pause_new_body_downloads(&mut self) {
        self.clear_needed_heights();
        self.state.schedule.clear_queued();
    }

    fn body_lag(&self) -> u32 {
        self.state
            .best_header_tip
            .0
            .saturating_sub(self.state.verified_block_tip.0)
    }

    fn should_pause_new_body_downloads(&self) -> bool {
        let lag = self.body_lag();
        lag == 0
            || lag <= self.startup.config.near_tip_body_download_pause_blocks
            || self.state.budget.available() == 0
    }

    fn release_caught_up_block_sync_peers(&mut self) {
        // Keep block-sync streams open even when this node is locally caught
        // up. A synced node can still be the server a fresh peer needs for
        // historical bodies, and closing the stream after every local catch-up
        // starves fresh Zakura-only nodes between checkpoint windows.
    }

    async fn handle_wire_decode_failed(
        &mut self,
        peer: ZakuraPeerId,
        error: Arc<BlockSyncWireError>,
    ) {
        tracing::debug!(?peer, ?error, "malformed Zakura block-sync frame");
        self.report_misbehavior(peer, BlockSyncMisbehavior::MalformedMessage)
            .await;
    }

    async fn handle_wire_message(&mut self, peer: ZakuraPeerId, msg: BlockSyncMessage) {
        if self.state.parked_peers.contains(&peer) {
            return;
        }

        match msg {
            BlockSyncMessage::Status(status) => self.handle_status(peer, status).await,
            BlockSyncMessage::Block(block) => self.handle_block(peer, block).await,
            BlockSyncMessage::BlocksDone {
                start_height,
                returned: _,
            } => self.handle_blocks_done(peer, start_height).await,
            BlockSyncMessage::RangeUnavailable {
                start_height,
                count,
            } => {
                self.handle_range_unavailable(peer, start_height, count)
                    .await;
            }
            BlockSyncMessage::GetBlocks {
                start_height,
                count,
            } => {
                self.handle_get_blocks(peer, start_height, count).await;
            }
        }
    }

    async fn handle_status(&mut self, peer: ZakuraPeerId, status: BlockSyncStatus) {
        if status.servable_low > status.servable_high {
            self.report_misbehavior(peer, BlockSyncMisbehavior::InvalidStatus)
                .await;
            return;
        }
        let now = Instant::now();
        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            return;
        };
        let servable_range_grew = status.servable_high > peer_state.servable_high
            || status.servable_low < peer_state.servable_low;
        if !peer_state.inbound_status.try_take(now) && !servable_range_grew {
            self.report_misbehavior(peer, BlockSyncMisbehavior::StatusSpam)
                .await;
            return;
        }
        let send_status_reply = peer_state.unsolicited.try_take(now);
        peer_state.servable_low = status.servable_low;
        peer_state.servable_high = status.servable_high;
        peer_state.max_blocks_per_response =
            clamp_advertised_blocks(status.max_blocks_per_response);
        peer_state.max_inflight_requests = clamp_advertised_inflight(status.max_inflight_requests);
        peer_state.max_response_bytes = clamp_advertised_response_bytes(status.max_response_bytes);
        peer_state.received_status = true;
        self.trace_status_received(&peer, status);
        self.publish_candidate_state();
        if send_status_reply {
            self.send_status(&peer, "status_reply").await;
        }
        self.schedule().await;
    }

    async fn handle_block(&mut self, peer: ZakuraPeerId, block: Arc<block::Block>) {
        let hash = block.hash();
        let Some(height) = block.coinbase_height() else {
            self.report_misbehavior(peer, BlockSyncMisbehavior::InvalidBlock)
                .await;
            return;
        };

        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            if self.ignore_disconnected_peer_response(&peer, "body") {
                return;
            }
            if self
                .ignore_stale_response(&peer, height, "body from inactive peer")
                .await
            {
                return;
            }
            self.report_misbehavior(peer, BlockSyncMisbehavior::UnsolicitedBlock)
                .await;
            return;
        };
        let Some(index) = peer_state.outstanding_index_for_height(height) else {
            if self.ignore_stale_response(&peer, height, "body").await {
                return;
            }
            if self
                .ignore_unmatched_needed_response(&peer, height, "body")
                .await
            {
                return;
            }
            if self
                .ignore_unmatched_active_body_response(&peer, height, hash)
                .await
            {
                return;
            }
            self.report_misbehavior(peer, BlockSyncMisbehavior::UnsolicitedBlock)
                .await;
            return;
        };
        let outstanding = &peer_state.outstanding[index];
        if outstanding.has_received(height) {
            tracing::debug!(?peer, ?height, "ignoring duplicate block-sync body frame");
            return;
        }

        if outstanding.request.expected_hash(height) != Some(hash) {
            self.report_misbehavior(peer, BlockSyncMisbehavior::InvalidBlock)
                .await;
            return;
        }
        let estimated_bytes = outstanding.estimated_bytes_for_height(height).unwrap_or(0);
        let retry_request = outstanding.request.single_height_retry(height);

        // The body's transactions are not validated against the header here:
        // recomputing the merkle root for every received body (including the
        // common valid case) was measurably expensive, so it is left to
        // consensus. A body whose merkle root does not match its header is
        // rejected by consensus during apply, and `handle_block_apply_finished`
        // attributes that rejection back to the delivering peer for misbehavior
        // scoring (see the `source_peer` plumbing through the reorder buffer).

        let serialized_bytes = match block.zcash_serialize_to_vec() {
            Ok(bytes) => bytes.len() as u64,
            Err(error) => {
                tracing::debug!(?error, "failed to serialize decoded block-sync body");
                self.finish_peer_outstanding_at(
                    &peer,
                    index,
                    OutstandingRangeDisposition::RetryOriginal,
                );
                self.report_misbehavior(peer, BlockSyncMisbehavior::InvalidBlock)
                    .await;
                return;
            }
        };
        if serialized_bytes
            > tolerated_bytes(
                estimated_bytes,
                self.startup.config.size_deviation_tolerance,
            )
        {
            self.report_misbehavior(peer.clone(), BlockSyncMisbehavior::SizeMismatch)
                .await;
        }

        metrics::counter!("sync.block.body.received").increment(1);
        self.trace_body_received(&peer, height, serialized_bytes);
        self.state.budget.release(estimated_bytes);
        let mut completed = None;
        if let Some(peer_state) = self.state.peers.get_mut(&peer) {
            if let Some(outstanding) = peer_state.outstanding.get_mut(index) {
                outstanding.mark_received(height);
                if outstanding.is_complete() {
                    completed = Some(peer_state.outstanding.remove(index));
                }
            }
        }
        if let Some(outstanding) = completed {
            self.finish_detached_outstanding(outstanding, OutstandingRangeDisposition::Satisfied);
        }

        if height <= self.state.body_download_floor
            || self.state.reorder.contains(height)
            || self.state.applying.contains_key(&height)
            || self.has_submitted_apply(height, hash)
        {
            self.release_contiguous_blocks().await;
            self.schedule().await;
            self.release_caught_up_block_sync_peers();
            return;
        }

        match self.state.reorder.insert(
            height,
            block,
            serialized_bytes,
            peer.clone(),
            &mut self.state.budget,
        ) {
            ReorderInsertResult::Inserted => {
                // A received body is now held in memory. Mark it covered so the
                // retry path stops re-requesting it. `refresh_needed` already
                // drops buffered heights from `needed`, but the retry path
                // (`handle_timeouts` / `handle_blocks_done` -> `retry`) bypasses
                // that filter and re-queues buffered heights via `push_front`.
                // A run buffered above an open gap was otherwise re-fetched
                // indefinitely (in production a single height was re-requested
                // thousands of times), pinning the queue front and every peer
                // slot so the gap below the run never got a request and the
                // download floor never advanced. `retry`, `ensure`, and
                // `prune_covered` all skip covered heights, so this stops the
                // churn and lets the gap be scheduled. Covered is cleared if the
                // block is later rolled back (apply `Rejected`/`TimedOut` ->
                // `clear_covered_from`) or the chain tip resets, both of which
                // also drop the buffer, so a dropped body becomes requestable
                // again.
                self.state.schedule.mark_height_covered(height);
            }
            ReorderInsertResult::Duplicate => {}
            ReorderInsertResult::BudgetFull => {
                if let Some(request) = retry_request {
                    self.state.schedule.retry(request);
                }
                tracing::debug!(
                    ?peer,
                    ?height,
                    serialized_bytes,
                    "dropping block-sync body because local byte budget is full"
                );
                self.schedule().await;
                return;
            }
        }
        self.release_contiguous_blocks().await;
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
    }

    async fn handle_get_blocks(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        count: u32,
    ) {
        let local_inflight_cap = self.startup.config.advertised_max_inflight_requests();
        let Some(peer_state) = self.state.peers.get_mut(&peer) else {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksSpam)
                .await;
            return;
        };

        if !peer_state.received_status {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksSpam)
                .await;
            return;
        }

        if count == 0 {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksTooLong)
                .await;
            return;
        }

        if !peer_state.try_start_serving_blocks(local_inflight_cap) {
            let unavailable_count = count.min(inbound_get_blocks_count_limit(&self.startup.config));
            self.send_range_unavailable(&peer, start_height, unavailable_count);
            return;
        }

        let requested_count = self.clamp_served_block_count(start_height, count);
        if requested_count == 0 {
            let unavailable_count = count.min(inbound_get_blocks_count_limit(&self.startup.config));
            self.send_range_unavailable(&peer, start_height, unavailable_count);
            self.finish_serving_blocks(&peer);
            return;
        }

        if !self
            .dispatch_action(BlockSyncAction::QueryBlocksByHeightRange {
                peer: peer.clone(),
                start: start_height,
                count: requested_count,
            })
            .await
        {
            self.finish_serving_blocks(&peer);
        }
    }

    fn finish_peer_outstanding_at(
        &mut self,
        peer: &ZakuraPeerId,
        index: usize,
        disposition: OutstandingRangeDisposition,
    ) {
        let Some(peer_state) = self.state.peers.get_mut(peer) else {
            return;
        };
        if index >= peer_state.outstanding.len() {
            return;
        }

        let outstanding = peer_state.outstanding.remove(index);
        self.finish_detached_outstanding(outstanding, disposition);
    }

    fn finish_detached_outstanding(
        &mut self,
        outstanding: OutstandingBlockRange,
        disposition: OutstandingRangeDisposition,
    ) {
        self.state.budget.release(outstanding.reserved_bytes());
        match disposition {
            OutstandingRangeDisposition::Satisfied => {
                self.state.schedule.clear_assignment(&outstanding.request);
            }
            OutstandingRangeDisposition::RetryOriginal => {
                self.state.schedule.retry(outstanding.request);
            }
            OutstandingRangeDisposition::RetryMissing => {
                self.state.schedule.clear_assignment(&outstanding.request);
                for request in outstanding
                    .missing_retry_requests(|height, hash| {
                        self.should_retry_missing_height(height, hash)
                    })
                    .into_iter()
                    .rev()
                {
                    self.state.schedule.retry(request);
                }
            }
        }
    }

    async fn handle_range_unavailable(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        _count: u32,
    ) {
        let Some(index) = self.outstanding_index_for_start(&peer, start_height) else {
            if self.ignore_disconnected_peer_response(&peer, "unavailable range") {
                return;
            }
            if self
                .ignore_stale_response(&peer, start_height, "unavailable range")
                .await
            {
                return;
            }

            self.trace_range_unavailable(&peer, start_height);
            return;
        };

        self.trace_range_unavailable(&peer, start_height);
        let disposition = self.stale_adjusted_outstanding_disposition(
            &peer,
            index,
            OutstandingRangeDisposition::RetryOriginal,
        );
        self.finish_peer_outstanding_at(&peer, index, disposition);
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
    }

    fn outstanding_index_for_start(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
    ) -> Option<usize> {
        self.state
            .peers
            .get(peer)?
            .outstanding_index_for_start(start_height)
    }

    fn is_stale_response_height(&self, height: block::Height) -> bool {
        height <= self.state.body_download_floor
            || self.state.reorder.contains(height)
            || self.state.applying.contains_key(&height)
    }

    async fn ignore_stale_response(
        &mut self,
        peer: &ZakuraPeerId,
        height: block::Height,
        response_kind: &'static str,
    ) -> bool {
        if !self.is_stale_response_height(height) {
            return false;
        }

        tracing::debug!(
            ?peer,
            ?height,
            response_kind,
            "ignoring stale block-sync response"
        );
        self.release_contiguous_blocks().await;
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
        true
    }

    async fn ignore_unmatched_needed_response(
        &mut self,
        peer: &ZakuraPeerId,
        height: block::Height,
        response_kind: &'static str,
    ) -> bool {
        if self.state.needed_heights.binary_search(&height).is_err() {
            return false;
        }

        metrics::counter!("sync.block.response.unmatched_needed_ignored").increment(1);
        tracing::debug!(
            ?peer,
            ?height,
            response_kind,
            "ignoring unmatched block-sync response for currently needed height"
        );
        self.release_contiguous_blocks().await;
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
        true
    }

    async fn ignore_unmatched_active_body_response(
        &mut self,
        peer: &ZakuraPeerId,
        height: block::Height,
        hash: block::Hash,
    ) -> bool {
        if !self.has_outstanding_request(height, hash) {
            return false;
        }

        metrics::counter!("sync.block.response.unmatched_active_ignored").increment(1);
        tracing::debug!(
            ?peer,
            ?height,
            "ignoring unmatched block-sync body for height active on another request"
        );
        self.release_contiguous_blocks().await;
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
        true
    }

    async fn ignore_unmatched_active_terminator_response(
        &mut self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
    ) -> bool {
        if !self.has_outstanding_request_start(start_height) {
            return false;
        }

        metrics::counter!("sync.block.response.unmatched_active_done_ignored").increment(1);
        tracing::debug!(
            ?peer,
            ?start_height,
            "ignoring unmatched block-sync terminator for range active on another request"
        );
        self.release_contiguous_blocks().await;
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
        true
    }

    fn ignore_disconnected_peer_response(
        &self,
        peer: &ZakuraPeerId,
        response_kind: &'static str,
    ) -> bool {
        if !self.state.disconnected_peers.contains(peer) {
            return false;
        }

        metrics::counter!("sync.block.response.disconnected_peer_ignored").increment(1);
        tracing::debug!(
            ?peer,
            response_kind,
            "ignoring late block-sync response from disconnected peer"
        );
        true
    }

    async fn handle_blocks_done(&mut self, peer: ZakuraPeerId, start_height: block::Height) {
        if !self.state.peers.contains_key(&peer) {
            if self.ignore_disconnected_peer_response(&peer, "terminator") {
                return;
            }
            if self
                .ignore_stale_response(&peer, start_height, "terminator from inactive peer")
                .await
            {
                return;
            }

            self.report_misbehavior(peer, BlockSyncMisbehavior::UnsolicitedDone)
                .await;
            return;
        }

        let Some(index) = self.outstanding_index_for_start(&peer, start_height) else {
            if self
                .ignore_stale_response(&peer, start_height, "terminator")
                .await
            {
                return;
            }
            if self
                .ignore_unmatched_needed_response(&peer, start_height, "terminator")
                .await
            {
                return;
            }
            if self
                .ignore_unmatched_active_terminator_response(&peer, start_height)
                .await
            {
                return;
            }
            // A known, active peer sent a response terminator that correlates to no
            // outstanding range. Fail closed: report `UnsolicitedDone` (a hard
            // block-sync misbehavior) instead of silently rescheduling.
            self.report_misbehavior(peer, BlockSyncMisbehavior::UnsolicitedDone)
                .await;
            return;
        };
        let disposition = self.stale_adjusted_outstanding_disposition(
            &peer,
            index,
            OutstandingRangeDisposition::RetryMissing,
        );
        self.finish_peer_outstanding_at(&peer, index, disposition);
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
    }

    fn stale_adjusted_outstanding_disposition(
        &mut self,
        peer: &ZakuraPeerId,
        index: usize,
        current: OutstandingRangeDisposition,
    ) -> OutstandingRangeDisposition {
        let tip = self.state.body_download_floor;
        let Some(peer_state) = self.state.peers.get_mut(peer) else {
            return current;
        };
        let Some(outstanding) = peer_state.outstanding.get_mut(index) else {
            return current;
        };
        if outstanding.request.start_height > tip {
            return current;
        }

        // A late response can still match an outstanding request after the
        // verified/download floor has moved through its prefix. Do not retry
        // already-committed heights; mark the stale prefix as satisfied and
        // retry only any remaining suffix.
        let released_bytes = outstanding.mark_received_through(tip);
        self.state.budget.release(released_bytes);
        if outstanding.is_complete() {
            OutstandingRangeDisposition::Satisfied
        } else {
            OutstandingRangeDisposition::RetryMissing
        }
    }

    async fn handle_block_range_response_ready(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        blocks: Vec<(block::Height, Arc<block::Block>, usize)>,
    ) {
        let max_response_bytes = u64::from(self.startup.config.advertised_max_response_bytes());
        let mut sent_blocks = 0u32;
        let mut sent_bytes = 0u64;
        let mut reason = "complete";

        for (height, block, size) in blocks {
            let Ok(size) = u64::try_from(size) else {
                reason = "size_overflow";
                break;
            };
            let Some(next_bytes) = sent_bytes.checked_add(size) else {
                reason = "byte_overflow";
                break;
            };
            if next_bytes > max_response_bytes {
                reason = "byte_cap";
                break;
            }
            if height_after_count(start_height, sent_blocks) != Some(height) {
                reason = "non_contiguous";
                break;
            }

            if !self.send_block(&peer, block).await {
                reason = "send_failed";
                break;
            }
            sent_blocks = sent_blocks.saturating_add(1);
            sent_bytes = next_bytes;
        }

        if sent_blocks == 0 {
            self.send_range_unavailable_wait(&peer, start_height, requested_count)
                .await;
        } else {
            self.send_blocks_done_wait(&peer, start_height, sent_blocks)
                .await;
        }
        self.trace_range_response_sent(
            &peer,
            start_height,
            requested_count,
            sent_blocks,
            sent_bytes,
            reason,
        );
        self.finish_serving_blocks(&peer);
    }

    async fn handle_block_range_response_finished(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        returned_count: u32,
    ) {
        if returned_count == 0 {
            self.send_range_unavailable(&peer, start_height, requested_count);
        }
        self.finish_serving_blocks(&peer);
    }

    async fn handle_block_apply_finished(
        &mut self,
        token: BlockApplyToken,
        height: block::Height,
        hash: block::Hash,
        result: BlockApplyResult,
        local_frontier: Option<BlockSyncFrontiers>,
    ) {
        let Some(applying) = self.state.applying.get(&height) else {
            self.decrement_submitted_apply(height, hash);
            return;
        };
        if applying.hash != hash || applying.token != token {
            self.decrement_submitted_apply(height, hash);
            return;
        }

        let (accepted_local_frontier, old_serving_tip) = if let Some(frontiers) = local_frontier {
            let old_serving_tip = self.apply_state_frontiers_changed(frontiers, false).await;
            (old_serving_tip.map(|_| frontiers), old_serving_tip)
        } else {
            (None, None)
        };

        if matches!(result, BlockApplyResult::Duplicate) && self.state.verified_block_tip < height {
            if let Some(old_serving_tip) = old_serving_tip {
                self.finish_frontier_update(old_serving_tip).await;
            }
            return;
        }
        let applying = self
            .state
            .applying
            .remove(&height)
            .expect("applying entry exists because it was just checked");

        self.state.budget.release(applying.bytes);
        self.decrement_submitted_apply(height, hash);
        self.trace_apply_finished(height, token, result);
        match result {
            BlockApplyResult::Committed | BlockApplyResult::Duplicate => {}
            BlockApplyResult::Rejected | BlockApplyResult::TimedOut
                if height > self.state.verified_block_tip =>
            {
                self.release_applying_blocks_from(height);
                self.state.body_download_floor = previous_height(height)
                    .unwrap_or(block::Height::MIN)
                    .max(self.state.verified_block_tip);
                self.state.schedule.clear_covered_from(height);
                self.state.reorder.drop_from(height, &mut self.state.budget);
                // A `Rejected` result means consensus found the body invalid
                // (e.g. a merkle root that does not match the header). Attribute
                // it to the peer that delivered the body so repeat offenders are
                // scored and eventually disconnected, rather than being free to
                // keep feeding invalid bodies for needed heights. `TimedOut` is a
                // local apply timeout, not a peer fault, so it is not scored.
                if matches!(result, BlockApplyResult::Rejected) {
                    self.report_misbehavior(
                        applying.source_peer.clone(),
                        BlockSyncMisbehavior::InvalidBlock,
                    )
                    .await;
                }
            }
            BlockApplyResult::Rejected | BlockApplyResult::TimedOut => {}
        }
        if let Some(frontiers) = accepted_local_frontier {
            self.release_applied_blocks_through(frontiers.verified_block_tip);
        }

        self.release_contiguous_blocks().await;
        if let Some(old_serving_tip) = old_serving_tip {
            self.queue_status_refresh_if_changed(old_serving_tip);
            self.flush_status_refresh().await;
        }
        if !self.query_needed_blocks().await {
            self.pause_new_body_downloads();
        }
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
    }

    fn finish_serving_blocks(&mut self, peer: &ZakuraPeerId) {
        if let Some(peer_state) = self.state.peers.get_mut(peer) {
            peer_state.finish_serving_blocks();
        }
    }

    async fn handle_timeouts(&mut self) {
        let now = Instant::now();
        let mut timed_out = Vec::new();
        for peer in self.state.peers.values_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if peer.outstanding[index].deadline <= now {
                    timed_out.push(peer.outstanding.remove(index));
                } else {
                    index += 1;
                }
            }
        }

        for outstanding in timed_out {
            self.finish_detached_outstanding(
                outstanding,
                OutstandingRangeDisposition::RetryOriginal,
            );
        }
        self.schedule().await;
        self.release_caught_up_block_sync_peers();
    }

    async fn query_needed_blocks(&mut self) -> bool {
        if !self.startup.state_queries_enabled || self.should_pause_new_body_downloads() {
            return false;
        }
        if self.local_body_work_blocks() >= self.refill_low_water_blocks() {
            return true;
        }
        let _ = self
            .dispatch_action(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: self.state.body_download_floor,
                best_header_tip: self.state.best_header_tip,
            })
            .await;
        true
    }

    fn local_body_work_blocks(&self) -> usize {
        let outstanding: usize = self
            .state
            .peers
            .values()
            .map(|peer| {
                peer.outstanding
                    .iter()
                    .map(|outstanding| {
                        outstanding
                            .request
                            .expected_hashes
                            .len()
                            .saturating_sub(outstanding.received.len())
                    })
                    .sum::<usize>()
            })
            .sum();

        self.state
            .schedule
            .queued_block_count()
            .saturating_add(outstanding)
            .saturating_add(self.state.reorder.len())
            .saturating_add(self.state.applying.len())
    }

    fn refill_low_water_blocks(&self) -> usize {
        let status_peers = self
            .state
            .peers
            .values()
            .filter(|peer| peer.received_status)
            .count()
            .max(1);
        let max_blocks_per_response =
            usize::try_from(self.startup.config.advertised_max_blocks_per_response())
                .expect("advertised block count fits usize");
        let max_inflight_per_peer =
            usize::from(self.startup.config.advertised_max_inflight_requests())
                .min(EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER);

        status_peers
            .saturating_mul(max_inflight_per_peer)
            .saturating_mul(max_blocks_per_response)
            .max(max_blocks_per_response)
    }

    fn drop_ranges_not_in_needed(&mut self, needed: &HashMap<block::Height, block::Hash>) {
        let mut dropped = Vec::new();
        for peer in self.state.peers.values_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if peer.outstanding[index].request.matches_needed(needed) {
                    index += 1;
                } else {
                    dropped.push(peer.outstanding.remove(index));
                }
            }
        }

        for outstanding in dropped {
            self.finish_detached_outstanding(outstanding, OutstandingRangeDisposition::Satisfied);
        }
    }

    fn needed_hashes_with_active_outstanding(
        &self,
        needed: &HashMap<block::Height, block::Hash>,
    ) -> HashMap<block::Height, block::Hash> {
        let mut retained = needed.clone();
        for peer in self.state.peers.values() {
            for outstanding in &peer.outstanding {
                for (height, hash) in &outstanding.request.expected_hashes {
                    retained.entry(*height).or_insert(*hash);
                }
            }
        }
        retained
    }

    fn drop_outstanding_through(&mut self, tip: block::Height) {
        let mut completed = Vec::new();
        let mut released_bytes = 0;
        for peer in self.state.peers.values_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if peer.outstanding[index].request.start_height <= tip {
                    released_bytes += peer.outstanding[index].mark_received_through(tip);
                    if peer.outstanding[index].is_complete() {
                        completed.push(peer.outstanding.remove(index));
                    } else {
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
        }

        self.state.budget.release(released_bytes);
        for outstanding in completed {
            self.finish_detached_outstanding(outstanding, OutstandingRangeDisposition::Satisfied);
        }
    }

    async fn schedule(&mut self) {
        self.submit_pending_blocks().await;

        if self.should_pause_new_body_downloads() {
            let reason = if self.body_lag() == 0 {
                "lag_zero"
            } else if self.body_lag() <= self.startup.config.near_tip_body_download_pause_blocks {
                "near_tip"
            } else {
                "budget_full"
            };
            self.trace_downloads_paused(reason);
            return;
        }

        let mut peer_ids: Vec<_> = self.state.peers.keys().cloned().collect();
        peer_ids.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        let per_peer_byte_cap = self.startup.config.per_peer_byte_cap();

        for peer_id in peer_ids {
            // Fill this peer's available slots in one pass, letting the byte
            // budget (re-checked each iteration) be the congestion window. A
            // raised slot cap is only useful if we can open the window promptly
            // rather than one slot per scheduling event.
            loop {
                if self.should_pause_new_body_downloads() {
                    return;
                }
                let Some(peer) = self.state.peers.get(&peer_id) else {
                    break;
                };
                if !peer.received_status || peer.available_slots() == 0 {
                    break;
                }
                let request = match self.state.schedule.next_for_peer(
                    &peer_id,
                    peer,
                    &mut self.state.budget,
                    per_peer_byte_cap,
                ) {
                    Ok(request) => request,
                    Err(reason) => {
                        self.trace_schedule_skipped(&peer_id, reason);
                        break;
                    }
                };

                let Some(peer) = self.state.peers.get(&peer_id) else {
                    break;
                };
                if let Err(error) = peer
                    .session
                    .try_send_get_blocks(request.start_height, request.count)
                {
                    tracing::debug!(
                        peer = ?peer_id,
                        start_height = ?request.start_height,
                        count = request.count,
                        ?error,
                        "failed to queue Zakura block-sync GetBlocks"
                    );
                    peer.session.cancel_token().cancel();
                    self.state.budget.release(request.estimated_bytes);
                    self.state.schedule.retry(request);
                    break;
                }

                metrics::counter!("sync.block.request.sent").increment(1);
                self.trace_get_blocks_sent(
                    &peer_id,
                    request.start_height,
                    request.count,
                    request.estimated_bytes,
                );
                let deadline = Instant::now() + self.startup.config.request_timeout;
                if let Some(peer) = self.state.peers.get_mut(&peer_id) {
                    peer.outstanding.push(OutstandingBlockRange {
                        request: request.clone(),
                        deadline,
                        received: HashSet::new(),
                    });
                }
                let _ = self
                    .dispatch_action(BlockSyncAction::SendMessage {
                        peer: peer_id.clone(),
                        msg: BlockSyncMessage::GetBlocks {
                            start_height: request.start_height,
                            count: request.count,
                        },
                    })
                    .await;
            }
        }
    }

    async fn release_contiguous_blocks(&mut self) {
        let released = self
            .state
            .reorder
            .drain_contiguous_prefix(self.state.body_download_floor);
        for (height, block, bytes, source_peer) in released {
            let hash = block.hash();
            self.state.body_download_floor = height;
            self.state.schedule.mark_height_covered(height);
            self.state.applying.insert(
                height,
                ApplyingBlock {
                    token: 0,
                    hash,
                    block,
                    bytes,
                    submitted: false,
                    source_peer,
                },
            );
        }

        self.submit_pending_blocks().await;
    }

    async fn submit_pending_blocks(&mut self) {
        let submitted = self
            .state
            .applying
            .values()
            .filter(|applying| applying.submitted)
            .count();
        let available = self
            .startup
            .config
            .submitted_apply_limit()
            .saturating_sub(submitted);
        if available == 0 {
            return;
        }

        let pending: Vec<_> = self
            .state
            .applying
            .iter()
            .filter_map(|(height, applying)| (!applying.submitted).then_some(*height))
            .take(available)
            .collect();

        for height in pending {
            let Some(block) = self
                .state
                .applying
                .get(&height)
                .map(|applying| applying.block.clone())
            else {
                continue;
            };

            let token = self.next_apply_token();
            if let Some(applying) = self.state.applying.get_mut(&height) {
                applying.token = token;
                applying.submitted = true;
            }

            metrics::counter!("sync.block.submit.sent").increment(1);
            if !self
                .dispatch_action(BlockSyncAction::SubmitBlock { token, block })
                .await
            {
                if let Some(applying) = self.state.applying.get_mut(&height) {
                    if applying.token == token {
                        applying.token = 0;
                        applying.submitted = false;
                    }
                }
                return;
            }
            if let Some(applying) = self.state.applying.get(&height) {
                self.increment_submitted_apply(height, applying.hash);
            }
            self.trace_body_submitted(height, token);
        }
    }

    fn next_apply_token(&mut self) -> BlockApplyToken {
        let token = self.state.next_apply_token;
        self.state.next_apply_token = self.state.next_apply_token.checked_add(1).unwrap_or(1);
        token
    }

    fn has_submitted_apply(&self, height: block::Height, hash: block::Hash) -> bool {
        self.state
            .submitted_applies
            .get(&height)
            .is_some_and(|entries| entries.iter().any(|(entry_hash, _)| *entry_hash == hash))
    }

    fn has_outstanding_request(&self, height: block::Height, hash: block::Hash) -> bool {
        self.state.peers.values().any(|peer| {
            peer.outstanding
                .iter()
                .any(|outstanding| outstanding.request.expected_hash(height) == Some(hash))
        })
    }

    fn has_outstanding_request_start(&self, start_height: block::Height) -> bool {
        self.state.peers.values().any(|peer| {
            peer.outstanding
                .iter()
                .any(|outstanding| outstanding.request.start_height == start_height)
        })
    }

    fn should_retry_missing_height(&self, height: block::Height, hash: block::Hash) -> bool {
        height > self.state.body_download_floor
            && !self.state.reorder.contains(height)
            && !self.state.applying.contains_key(&height)
            && !self.has_submitted_apply(height, hash)
            && !self.has_outstanding_request(height, hash)
    }

    fn has_active_successor_after(&self, height: block::Height) -> bool {
        let Some(next) = next_height(height) else {
            return false;
        };

        self.state.reorder.contains_at_or_above(next)
            || self.state.applying.range(next..).next().is_some()
            || self.state.submitted_applies.range(next..).next().is_some()
            || self.state.peers.values().any(|peer| {
                peer.outstanding
                    .iter()
                    .any(|outstanding| outstanding.request.end_height() >= next)
            })
    }

    /// Returns `false` only when we hold the direct successor body (in
    /// `applying`) at `height + 1` and that body's `previous_block_hash` does
    /// not link to `anchor_hash`.
    ///
    /// A reset that lands on a tip our already-submitted successor builds on is
    /// non-destructive growth/coalescing: the successor is still valid work for
    /// the new anchor, so it must be preserved. A reset that lands on a
    /// *different* tip hash orphans that successor — its parent is no longer the
    /// verified tip — so it must be dropped and re-requested against the new
    /// anchor. We can only make this distinction for bodies we actually hold;
    /// outstanding/buffered successors have no decoded header here, so they are
    /// treated as still-anchored (preserved) and re-validated on arrival.
    fn active_successor_links_to_anchor(
        &self,
        height: block::Height,
        anchor_hash: block::Hash,
    ) -> bool {
        let Some(next) = next_height(height) else {
            return true;
        };

        self.state
            .applying
            .get(&next)
            .map(|applying| applying.block.header.previous_block_hash == anchor_hash)
            .unwrap_or(true)
    }

    fn reset_tip_conflicts_with_local_work(
        &self,
        frontiers: &BlockSyncFrontiers,
        ignore_non_material_conflicts: bool,
    ) -> bool {
        let height = frontiers.verified_block_tip;
        let hash = frontiers.verified_block_hash;

        if self
            .state
            .reorder
            .hash(height)
            .is_some_and(|buffered_hash| buffered_hash != hash)
        {
            return true;
        }
        if self
            .state
            .applying
            .get(&height)
            .is_some_and(|applying| applying.hash != hash)
        {
            return true;
        }
        if !ignore_non_material_conflicts
            && self
                .state
                .submitted_applies
                .get(&height)
                .is_some_and(|entries| entries.iter().all(|(entry_hash, _)| *entry_hash != hash))
        {
            return true;
        }
        if !ignore_non_material_conflicts
            && self.state.peers.values().any(|peer| {
                peer.outstanding.iter().any(|outstanding| {
                    outstanding
                        .request
                        .expected_hash(height)
                        .is_some_and(|expected_hash| expected_hash != hash)
                })
            })
        {
            return true;
        }
        false
    }

    fn increment_submitted_apply(&mut self, height: block::Height, hash: block::Hash) {
        let entries = self.state.submitted_applies.entry(height).or_default();
        if let Some((_, count)) = entries
            .iter_mut()
            .find(|(entry_hash, _)| *entry_hash == hash)
        {
            *count = count.saturating_add(1);
        } else {
            entries.push((hash, 1));
        }
    }

    fn decrement_submitted_apply(&mut self, height: block::Height, hash: block::Hash) {
        let Some(entries) = self.state.submitted_applies.get_mut(&height) else {
            return;
        };
        if let Some(index) = entries
            .iter()
            .position(|(entry_hash, _)| *entry_hash == hash)
        {
            let (_, count) = &mut entries[index];
            *count = count.saturating_sub(1);
            if *count == 0 {
                entries.remove(index);
            }
        }
        if entries.is_empty() {
            self.state.submitted_applies.remove(&height);
        }
    }

    fn clear_submitted_applies_from(&mut self, from: block::Height) {
        let heights: Vec<_> = self
            .state
            .submitted_applies
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        for height in heights {
            self.state.submitted_applies.remove(&height);
        }
    }

    fn release_applied_blocks_through(&mut self, tip: block::Height) {
        let applied: Vec<_> = self
            .state
            .applying
            .range(..=tip)
            .map(|(height, _)| *height)
            .collect();
        for height in applied {
            if let Some(applying) = self.state.applying.remove(&height) {
                self.state.budget.release(applying.bytes);
            }
        }
    }

    fn release_all_applying_blocks_for_reset(&mut self, keep_submitted_applies: bool) {
        let bytes = self
            .state
            .applying
            .values()
            .map(|applying| applying.bytes)
            .sum();
        if !keep_submitted_applies {
            self.state.submitted_applies.clear();
        }
        self.state.budget.release(bytes);
        self.state.applying.clear();
    }

    fn release_applying_blocks_from(&mut self, from: block::Height) {
        let heights: Vec<_> = self
            .state
            .applying
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        for height in heights {
            if let Some(applying) = self.state.applying.remove(&height) {
                self.state.budget.release(applying.bytes);
            }
        }
        self.clear_submitted_applies_from(from);
    }

    async fn send_status(&self, peer: &ZakuraPeerId, reason: &'static str) {
        let Some(peer_state) = self.state.peers.get(peer) else {
            return;
        };
        let status = self.local_status();
        let session = peer_state.session.clone();
        if let Err(error) = session.try_send_status(status) {
            tracing::debug!(?peer, ?error, "failed to queue Zakura block-sync Status");
            self.trace_status_send_failed(peer, reason);
            session.cancel_token().cancel();
            return;
        }
        self.trace_status_sent(peer, reason, status);
        let _ = self
            .dispatch_action(BlockSyncAction::SendMessage {
                peer: peer.clone(),
                msg: BlockSyncMessage::Status(status),
            })
            .await;
    }

    async fn send_block(&self, peer: &ZakuraPeerId, block: Arc<block::Block>) -> bool {
        let Some(session) = self
            .state
            .peers
            .get(peer)
            .map(|peer_state| peer_state.session.clone())
        else {
            return false;
        };
        match time::timeout(ACTION_SEND_TIMEOUT, session.send_block(block)).await {
            Ok(Ok(())) => {
                metrics::counter!("sync.block.body.served").increment(1);
                true
            }
            Ok(Err(error)) => {
                tracing::debug!(?peer, ?error, "failed to queue Zakura block-sync Block");
                session.cancel_token().cancel();
                false
            }
            Err(_) => {
                metrics::counter!("sync.block.body.serve_timeout").increment(1);
                tracing::debug!(?peer, "timed out queueing Zakura block-sync Block");
                session.cancel_token().cancel();
                false
            }
        }
    }

    async fn send_blocks_done_wait(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        returned: u32,
    ) {
        if returned == 0 {
            return;
        }
        let Some(session) = self
            .state
            .peers
            .get(peer)
            .map(|peer_state| peer_state.session.clone())
        else {
            return;
        };
        match time::timeout(
            ACTION_SEND_TIMEOUT,
            session.send_blocks_done(start_height, returned),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(
                    ?peer,
                    ?error,
                    "failed to queue Zakura block-sync BlocksDone"
                );
                session.cancel_token().cancel();
            }
            Err(_) => {
                metrics::counter!("sync.block.done.serve_timeout").increment(1);
                tracing::debug!(?peer, "timed out queueing Zakura block-sync BlocksDone");
                session.cancel_token().cancel();
            }
        }
    }

    fn send_range_unavailable(&self, peer: &ZakuraPeerId, start_height: block::Height, count: u32) {
        let count = count.max(1);
        let Some(peer_state) = self.state.peers.get(peer) else {
            return;
        };
        if let Err(error) = peer_state
            .session
            .try_send_range_unavailable(start_height, count)
        {
            tracing::debug!(
                ?peer,
                ?error,
                "failed to queue Zakura block-sync RangeUnavailable"
            );
            peer_state.session.cancel_token().cancel();
        }
    }

    async fn send_range_unavailable_wait(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        count: u32,
    ) {
        let count = count.max(1);
        let Some(session) = self
            .state
            .peers
            .get(peer)
            .map(|peer_state| peer_state.session.clone())
        else {
            return;
        };
        match time::timeout(
            ACTION_SEND_TIMEOUT,
            session.send_range_unavailable(start_height, count),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(
                    ?peer,
                    ?error,
                    "failed to queue Zakura block-sync RangeUnavailable"
                );
                session.cancel_token().cancel();
            }
            Err(_) => {
                metrics::counter!("sync.block.unavailable.serve_timeout").increment(1);
                tracing::debug!(
                    ?peer,
                    "timed out queueing Zakura block-sync RangeUnavailable"
                );
                session.cancel_token().cancel();
            }
        }
    }

    async fn flush_status_refresh(&mut self) {
        let has_unready_peers = self.state.peers.values().any(|peer| !peer.received_status);
        if !self.state.pending_status_refresh && !has_unready_peers {
            return;
        }
        let now = Instant::now();

        // A genuine serving-range change is debounced by the global
        // `status_refresh` meter so a burst of tip changes advertises once per
        // window. Only consume that window when there is actually a change to
        // advertise: a flush that exists only to retry a Status to a peer that
        // has not acknowledged ours must not poison the change window, or the
        // first real advertisement after connect is silently dropped (the
        // connect-time retry would have already taken the window).
        let status = self.local_status();
        let status_changed = self.state.pending_status_refresh
            && status != self.state.last_advertised_status
            && self.state.status_refresh.try_take(now);

        self.state.pending_status_refresh = false;
        if status_changed {
            self.state.last_advertised_status = status;
            let _ = self.status.send(status);
        }

        let peer_ids: Vec<_> = self
            .state
            .peers
            .iter_mut()
            .filter_map(|(peer_id, peer)| {
                // On a real change, advertise to every peer immediately; the
                // global meter above already debounced the change, so the
                // per-peer `unsolicited` meter must not also suppress it. We
                // still consume the per-peer allowance so a same-window retry to
                // this peer stays spaced. Otherwise the only reason to send is a
                // retry to a peer that has not acknowledged our Status, which
                // stays gated solely by that peer's `unsolicited` meter.
                if status_changed {
                    peer.unsolicited.mark_taken(now);
                    Some(peer_id.clone())
                } else if !peer.received_status && peer.unsolicited.try_take(now) {
                    Some(peer_id.clone())
                } else {
                    None
                }
            })
            .collect();

        for peer in peer_ids {
            self.send_status(&peer, "refresh").await;
        }
    }

    fn queue_status_refresh_if_changed(&mut self, old_serving_tip: (block::Height, block::Hash)) {
        if old_serving_tip != (self.state.servable_high, self.state.servable_hash)
            && self.local_status() != self.state.last_advertised_status
        {
            self.state.pending_status_refresh = true;
        }
    }

    fn emit_trace(
        &self,
        event: &'static str,
        build: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        self.startup.trace.emit_with(BLOCK_SYNC_TABLE, |row| {
            row.insert(
                bs_trace::EVENT.to_string(),
                serde_json::Value::String(event.to_string()),
            );
            build(row);
        });
    }

    /// Emit the periodic reactor snapshot used to diagnose body-sync stalls.
    ///
    /// This is the highest-signal row: a stall shows up as `body_download_floor`
    /// and `verified_block_tip` frozen while `best_header_tip` climbs, plus
    /// whichever resource is pinned (`budget_available == 0`, `applying`/`reorder`
    /// growing, or `peers_with_status == 0`).
    fn trace_sync_state(&self) {
        if !self.startup.trace.is_enabled() {
            return;
        }
        let outstanding: usize = self
            .state
            .peers
            .values()
            .map(|peer| peer.outstanding.len())
            .sum();
        let peers_with_status = self
            .state
            .peers
            .values()
            .filter(|peer| peer.received_status)
            .count();
        let submitted_applies = self
            .state
            .applying
            .values()
            .filter(|applying| applying.submitted)
            .count();
        self.emit_trace(bs_trace::BLOCK_SYNC_STATE, |row| {
            bs_insert_height(
                row,
                bs_trace::BODY_DOWNLOAD_FLOOR,
                self.state.body_download_floor,
            );
            bs_insert_height(
                row,
                bs_trace::VERIFIED_BLOCK_TIP,
                self.state.verified_block_tip,
            );
            bs_insert_height(row, bs_trace::BEST_HEADER_TIP, self.state.best_header_tip);
            bs_insert_u64(row, bs_trace::BODY_LAG, u64::from(self.body_lag()));
            bs_insert_u64(row, bs_trace::APPLYING, self.state.applying.len() as u64);
            bs_insert_u64(row, bs_trace::SUBMITTED_APPLIES, submitted_applies as u64);
            bs_insert_u64(row, bs_trace::REORDER, self.state.reorder.len() as u64);
            bs_insert_u64(row, bs_trace::OUTSTANDING, outstanding as u64);
            bs_insert_u64(
                row,
                bs_trace::BUDGET_AVAILABLE,
                self.state.budget.available(),
            );
            bs_insert_u64(row, bs_trace::BUDGET_RESERVED, self.state.budget.reserved());
            bs_insert_u64(row, bs_trace::PEERS, self.state.peers.len() as u64);
            bs_insert_u64(row, bs_trace::PEERS_WITH_STATUS, peers_with_status as u64);
            // Scheduling visibility: distinguishes "gap not in `needed`"
            // (state/filter) from "gap in `needed` but never queued" (`ensure`
            // rejected it) from "queued but never requested" (starvation).
            if let Some(min) = self.state.needed_heights.first() {
                bs_insert_height(row, bs_trace::NEEDED_MIN, *min);
            }
            bs_insert_u64(
                row,
                bs_trace::NEEDED_COUNT,
                self.state.needed_heights.len() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::QUEUE_LEN,
                self.state.schedule.queued_range_count() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::QUEUE_BLOCKS,
                self.state.schedule.queued_block_count() as u64,
            );
            if let Some(start) = self.state.schedule.queued_min_start() {
                bs_insert_height(row, bs_trace::QUEUE_MIN_START, start);
            }
            bs_insert_u64(
                row,
                bs_trace::ASSIGNED_LEN,
                self.state.schedule.assigned_key_count() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::LOCAL_BODY_WORK,
                self.local_body_work_blocks() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::REFILL_LOW_WATER,
                self.refill_low_water_blocks() as u64,
            );
            if let Some(end) = self.state.schedule.covered_max_end() {
                bs_insert_height(row, bs_trace::COVERED_MAX_END, end);
            }
        });
    }

    fn trace_status_received(&self, peer: &ZakuraPeerId, status: BlockSyncStatus) {
        self.emit_trace(bs_trace::BLOCK_STATUS_RECEIVED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::RANGE_START, status.servable_low);
            bs_insert_height(row, bs_trace::HEIGHT, status.servable_high);
        });
    }

    fn trace_status_sent(
        &self,
        peer: &ZakuraPeerId,
        reason: &'static str,
        status: BlockSyncStatus,
    ) {
        self.emit_trace(bs_trace::BLOCK_STATUS_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_str(row, bs_trace::REASON, reason);
            bs_insert_height(row, bs_trace::RANGE_START, status.servable_low);
            bs_insert_height(row, bs_trace::HEIGHT, status.servable_high);
        });
    }

    fn trace_status_send_failed(&self, peer: &ZakuraPeerId, reason: &'static str) {
        self.emit_trace(bs_trace::BLOCK_STATUS_SEND_FAILED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_str(row, bs_trace::REASON, reason);
        });
    }

    fn trace_peer_connected(&self, peer: &ZakuraPeerId, direction: ServicePeerDirection) {
        self.emit_trace(bs_trace::BLOCK_PEER_CONNECTED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_str(row, "direction", direction.trace_label());
        });
    }

    fn trace_peer_disconnected(&self, peer: &ZakuraPeerId, received_status: bool) {
        self.emit_trace(bs_trace::BLOCK_PEER_DISCONNECTED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            row.insert(
                "received_status".to_string(),
                serde_json::Value::Bool(received_status),
            );
        });
    }

    fn trace_get_blocks_sent(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        count: u32,
        estimated_bytes: u64,
    ) {
        self.emit_trace(bs_trace::BLOCK_GET_BLOCKS_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(count));
            bs_insert_u64(row, bs_trace::ESTIMATED_BYTES, estimated_bytes);
        });
    }

    fn trace_body_received(
        &self,
        peer: &ZakuraPeerId,
        height: block::Height,
        serialized_bytes: u64,
    ) {
        self.emit_trace(bs_trace::BLOCK_BODY_RECEIVED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::SERIALIZED_BYTES, serialized_bytes);
        });
    }

    fn trace_body_submitted(&self, height: block::Height, token: BlockApplyToken) {
        self.emit_trace(bs_trace::BLOCK_BODY_SUBMITTED, |row| {
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::APPLY_TOKEN, token);
        });
    }

    fn trace_apply_finished(
        &self,
        height: block::Height,
        token: BlockApplyToken,
        result: BlockApplyResult,
    ) {
        self.emit_trace(bs_trace::BLOCK_APPLY_FINISHED, |row| {
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::APPLY_TOKEN, token);
            bs_insert_str(row, bs_trace::RESULT, block_apply_result_label(result));
        });
    }

    fn trace_range_unavailable(&self, peer: &ZakuraPeerId, start_height: block::Height) {
        self.emit_trace(bs_trace::BLOCK_RANGE_UNAVAILABLE, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
        });
    }

    fn trace_range_response_sent(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        sent_count: u32,
        sent_bytes: u64,
        reason: &'static str,
    ) {
        self.emit_trace(bs_trace::BLOCK_RANGE_RESPONSE_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(sent_count));
            bs_insert_u64(row, bs_trace::EXPECTED_COUNT, u64::from(requested_count));
            bs_insert_u64(row, bs_trace::SERIALIZED_BYTES, sent_bytes);
            bs_insert_str(row, bs_trace::REASON, reason);
        });
    }

    fn trace_downloads_paused(&self, reason: &'static str) {
        self.emit_trace(bs_trace::BLOCK_DOWNLOADS_PAUSED, |row| {
            bs_insert_str(row, bs_trace::REASON, reason);
            bs_insert_u64(row, bs_trace::BODY_LAG, u64::from(self.body_lag()));
            bs_insert_u64(
                row,
                bs_trace::BUDGET_AVAILABLE,
                self.state.budget.available(),
            );
        });
    }

    fn trace_schedule_skipped(&self, peer: &ZakuraPeerId, reason: ScheduleSkipReason) {
        self.emit_trace(bs_trace::BLOCK_SCHEDULE_SKIPPED, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_str(row, bs_trace::REASON, reason.as_str());
            if let Some(peer_state) = self.state.peers.get(peer) {
                bs_insert_u64(row, "available_slots", peer_state.available_slots() as u64);
                bs_insert_u64(row, "peer_outstanding", peer_state.outstanding.len() as u64);
                let peer_reserved = peer_state
                    .outstanding
                    .iter()
                    .map(|outstanding| outstanding.reserved_bytes())
                    .sum::<u64>();
                bs_insert_u64(row, "peer_reserved", peer_reserved);
                bs_insert_height(row, "servable_low", peer_state.servable_low);
                bs_insert_height(row, "servable_high", peer_state.servable_high);
            }
            bs_insert_u64(
                row,
                bs_trace::QUEUE_LEN,
                self.state.schedule.queued_range_count() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::QUEUE_BLOCKS,
                self.state.schedule.queued_block_count() as u64,
            );
            if let Some(start) = self.state.schedule.queued_min_start() {
                bs_insert_height(row, bs_trace::QUEUE_MIN_START, start);
            }
            bs_insert_u64(
                row,
                bs_trace::ASSIGNED_LEN,
                self.state.schedule.assigned_key_count() as u64,
            );
            bs_insert_u64(row, bs_trace::BUDGET_RESERVED, self.state.budget.reserved());
            bs_insert_u64(
                row,
                bs_trace::BUDGET_AVAILABLE,
                self.state.budget.available(),
            );
        });
    }

    fn trace_frontiers_changed(&self, verified_block_tip: block::Height) {
        self.emit_trace(bs_trace::BLOCK_FRONTIERS_CHANGED, |row| {
            bs_insert_height(row, bs_trace::VERIFIED_BLOCK_TIP, verified_block_tip);
            bs_insert_height(row, bs_trace::BEST_HEADER_TIP, self.state.best_header_tip);
        });
    }

    fn trace_chain_tip_reset(&self, verified_block_tip: block::Height) {
        self.emit_trace(bs_trace::BLOCK_CHAIN_TIP_RESET, |row| {
            bs_insert_height(row, bs_trace::VERIFIED_BLOCK_TIP, verified_block_tip);
        });
    }

    fn publish_metrics(&self) {
        // These lossy casts are metrics-only gauges; consensus and scheduling
        // continue to use the original integer values.
        metrics::gauge!("sync.block.best_header_tip.height")
            .set(self.state.best_header_tip.0 as f64);
        metrics::gauge!("sync.block.verified_tip.height")
            .set(self.state.verified_block_tip.0 as f64);
        metrics::gauge!("sync.block.missing_bodies").set(self.state.needed_heights.len() as f64);
        metrics::gauge!("sync.block.budget.reserved_bytes")
            .set(self.state.budget.reserved() as f64);
        metrics::gauge!("sync.block.reorder.buffered_bytes")
            .set(self.state.reorder.buffered_bytes() as f64);
        metrics::gauge!("sync.block.applying").set(self.state.applying.len() as f64);
        metrics::gauge!("sync.block.outstanding").set(
            self.state
                .peers
                .values()
                .map(|peer| peer.outstanding.len())
                .sum::<usize>() as f64,
        );
    }

    fn clamp_served_block_count(&self, start_height: block::Height, count: u32) -> u32 {
        if start_height > self.state.servable_high {
            return 0;
        }

        let available = self
            .state
            .servable_high
            .0
            .checked_sub(start_height.0)
            .and_then(|diff| diff.checked_add(1))
            .unwrap_or(0);

        count
            .min(inbound_get_blocks_count_limit(&self.startup.config))
            .min(available)
    }

    fn local_status(&self) -> BlockSyncStatus {
        BlockSyncStatus {
            servable_low: block::Height::MIN,
            servable_high: self.state.servable_high,
            tip_hash: self.state.servable_hash,
            max_blocks_per_response: self.startup.config.advertised_max_blocks_per_response(),
            max_inflight_requests: self.startup.config.advertised_max_inflight_requests(),
            max_response_bytes: self.startup.config.advertised_max_response_bytes(),
        }
    }

    /// Hand a data-plane action to the action driver without letting a slow or
    /// stalled driver wedge the reactor. Direct peer-session sends are already
    /// complete before `SendMessage` actions are mirrored, so those mirrors are
    /// dropped immediately if the channel is full. Required driver actions wait
    /// up to [`ACTION_SEND_TIMEOUT`]; past that the action is dropped so the
    /// reactor keeps draining peer-lifecycle events, request timeouts, and
    /// misbehavior disconnects. Returns `true` only if the action was accepted.
    async fn dispatch_action(&self, action: BlockSyncAction) -> bool {
        self.trace_action_dispatched(&action);
        if matches!(action, BlockSyncAction::SendMessage { .. }) {
            return match self.actions.try_send(action) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    metrics::counter!("sync.block.action.send_full_dropped").increment(1);
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            };
        }

        match time::timeout(ACTION_SEND_TIMEOUT, self.actions.send(action)).await {
            Ok(Ok(())) => true,
            // Receiver dropped: the driver is gone, treat like a send failure.
            Ok(Err(_)) => false,
            // Driver stalled past the deadline: drop the action and stay live.
            Err(_) => {
                metrics::counter!("sync.block.action.send_timeout").increment(1);
                false
            }
        }
    }

    async fn report_misbehavior(&mut self, peer: ZakuraPeerId, reason: BlockSyncMisbehavior) {
        let mut cancel_peer = None;
        if let Some(peer_state) = self.state.peers.get_mut(&peer) {
            peer_state.misbehavior = peer_state.misbehavior.saturating_add(1);
            if block_sync_misbehavior_is_soft(reason)
                && peer_state.misbehavior >= SOFT_MISBEHAVIOR_DISCONNECT_THRESHOLD
            {
                cancel_peer = Some(peer_state.session.cancel_token());
            }
        }
        if let Some(cancel_token) = cancel_peer {
            cancel_token.cancel();
        }
        // The Misbehavior action carries the hard-disconnect/scoring request to
        // the supervisor. Deliver it without ever blocking the reactor: awaiting
        // a full `actions` channel here was the backpressure stall that delayed
        // misbehavior disconnects, request timeouts, and lifecycle draining
        // whenever the action driver was slow. `try_send` keeps the reactor live
        // so it can promptly tear down soft offenders at threshold and deliver
        // the next disconnect as soon as the driver drains a slot.
        let action = BlockSyncAction::Misbehavior { peer, reason };
        self.trace_action_dispatched(&action);
        if self.actions.try_send(action).is_err() {
            metrics::counter!("sync.block.peer.disconnect.action_dropped").increment(1);
        }
    }

    fn trace_event_received(&self, event: &BlockSyncEvent) {
        self.emit_trace(bs_trace::BLOCK_EVENT_RECEIVED, |row| match event {
            BlockSyncEvent::PeerConnected(session) => {
                bs_insert_str(row, bs_trace::KIND, "peer_connected");
                bs_insert_peer(row, bs_trace::PEER, session.peer_id());
            }
            BlockSyncEvent::PeerDisconnected(peer) => {
                bs_insert_str(row, bs_trace::KIND, "peer_disconnected");
                bs_insert_peer(row, bs_trace::PEER, peer);
            }
            BlockSyncEvent::WireMessage { peer, msg } => {
                bs_insert_str(row, bs_trace::KIND, "wire_message");
                bs_insert_str(row, bs_trace::REASON, block_sync_message_label(msg));
                bs_insert_peer(row, bs_trace::PEER, peer);
                trace_block_sync_message_fields(row, msg);
            }
            BlockSyncEvent::WireDecodeFailed { peer, .. } => {
                bs_insert_str(row, bs_trace::KIND, "wire_decode_failed");
                bs_insert_peer(row, bs_trace::PEER, peer);
            }
            BlockSyncEvent::HeaderTipChanged { height, hash } => {
                bs_insert_str(row, bs_trace::KIND, "header_tip_changed");
                bs_insert_height(row, bs_trace::HEIGHT, *height);
                bs_insert_hash(row, bs_trace::HASH, *hash);
            }
            BlockSyncEvent::StateFrontiersChanged(frontiers) => {
                bs_insert_str(row, bs_trace::KIND, "state_frontiers_changed");
                bs_insert_frontiers(row, frontiers);
            }
            BlockSyncEvent::ChainTipGrow(frontiers) => {
                bs_insert_str(row, bs_trace::KIND, "chain_tip_grow");
                bs_insert_frontiers(row, frontiers);
            }
            BlockSyncEvent::ChainTipReset(frontiers) => {
                bs_insert_str(row, bs_trace::KIND, "chain_tip_reset");
                bs_insert_frontiers(row, frontiers);
            }
            BlockSyncEvent::NeededBlocks(blocks) => {
                bs_insert_str(row, bs_trace::KIND, "needed_blocks");
                bs_insert_u64(row, bs_trace::RANGE_COUNT, blocks.len() as u64);
                if let Some(first) = blocks.first() {
                    bs_insert_height(row, bs_trace::RANGE_START, first.height);
                }
            }
            BlockSyncEvent::BlockApplyFinished {
                token,
                height,
                hash,
                result,
                local_frontier,
            } => {
                bs_insert_str(row, bs_trace::KIND, "block_apply_finished");
                bs_insert_u64(row, bs_trace::APPLY_TOKEN, *token);
                bs_insert_height(row, bs_trace::HEIGHT, *height);
                bs_insert_hash(row, bs_trace::HASH, *hash);
                bs_insert_str(row, bs_trace::RESULT, block_apply_result_label(*result));
                if let Some(frontiers) = local_frontier {
                    bs_insert_frontiers(row, frontiers);
                }
            }
            BlockSyncEvent::BlockRangeResponseFinished {
                peer,
                start_height,
                requested_count,
                returned_count,
            } => {
                bs_insert_str(row, bs_trace::KIND, "block_range_response_finished");
                bs_insert_peer(row, bs_trace::PEER, peer);
                bs_insert_height(row, bs_trace::RANGE_START, *start_height);
                bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(*returned_count));
                bs_insert_u64(row, bs_trace::EXPECTED_COUNT, u64::from(*requested_count));
            }
            BlockSyncEvent::BlockRangeResponseReady {
                peer,
                start_height,
                requested_count,
                blocks,
            } => {
                bs_insert_str(row, bs_trace::KIND, "block_range_response_ready");
                bs_insert_peer(row, bs_trace::PEER, peer);
                bs_insert_height(row, bs_trace::RANGE_START, *start_height);
                bs_insert_u64(row, bs_trace::RANGE_COUNT, blocks.len() as u64);
                bs_insert_u64(row, bs_trace::EXPECTED_COUNT, u64::from(*requested_count));
            }
        });
    }

    fn trace_action_dispatched(&self, action: &BlockSyncAction) {
        self.emit_trace(bs_trace::BLOCK_ACTION_DISPATCHED, |row| match action {
            BlockSyncAction::SendMessage { peer, msg } => {
                bs_insert_str(row, bs_trace::KIND, "send_message");
                bs_insert_str(row, bs_trace::REASON, block_sync_message_label(msg));
                bs_insert_peer(row, bs_trace::PEER, peer);
                trace_block_sync_message_fields(row, msg);
            }
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                bs_insert_str(row, bs_trace::KIND, "query_needed_blocks");
                bs_insert_height(row, bs_trace::VERIFIED_BLOCK_TIP, *verified_block_tip);
                bs_insert_height(row, bs_trace::BEST_HEADER_TIP, *best_header_tip);
            }
            BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                bs_insert_str(row, bs_trace::KIND, "query_blocks_by_height_range");
                bs_insert_peer(row, bs_trace::PEER, peer);
                bs_insert_height(row, bs_trace::RANGE_START, *start);
                bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(*count));
            }
            BlockSyncAction::SubmitBlock { token, block } => {
                bs_insert_str(row, bs_trace::KIND, "submit_block");
                bs_insert_u64(row, bs_trace::APPLY_TOKEN, *token);
                bs_insert_hash(row, bs_trace::HASH, block.hash());
                if let Some(height) = block.coinbase_height() {
                    bs_insert_height(row, bs_trace::HEIGHT, height);
                }
            }
            BlockSyncAction::Misbehavior { peer, reason } => {
                bs_insert_str(row, bs_trace::KIND, "misbehavior");
                bs_insert_peer(row, bs_trace::PEER, peer);
                bs_insert_str(row, bs_trace::REASON, block_misbehavior_label(*reason));
            }
        });
    }
}

pub(super) fn node_id_from_block_peer_id(peer_id: &ZakuraPeerId) -> Option<NodeId> {
    let bytes: [u8; 32] = peer_id.as_bytes().try_into().ok()?;
    NodeId::from_bytes(&bytes).ok()
}

fn block_apply_result_label(result: BlockApplyResult) -> &'static str {
    match result {
        BlockApplyResult::Committed => "committed",
        BlockApplyResult::Duplicate => "duplicate",
        BlockApplyResult::Rejected => "rejected",
        BlockApplyResult::TimedOut => "timed_out",
    }
}

fn bs_insert_peer(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    peer: &ZakuraPeerId,
) {
    row.insert(
        key.to_string(),
        serde_json::Value::String(trace_peer_label(peer)),
    );
}

fn bs_insert_height(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    height: block::Height,
) {
    bs_insert_u64(row, key, u64::from(height.0));
}

fn bs_insert_hash(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    hash: block::Hash,
) {
    row.insert(
        key.to_string(),
        serde_json::Value::String(format!("{hash}")),
    );
}

fn bs_insert_u64(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    value: u64,
) {
    row.insert(key.to_string(), serde_json::Value::from(value));
}

fn bs_insert_frontiers(
    row: &mut serde_json::Map<String, serde_json::Value>,
    frontiers: &BlockSyncFrontiers,
) {
    bs_insert_height(
        row,
        bs_trace::VERIFIED_BLOCK_TIP,
        frontiers.verified_block_tip,
    );
    bs_insert_hash(row, bs_trace::HASH, frontiers.verified_block_hash);
}

fn trace_block_sync_message_fields(
    row: &mut serde_json::Map<String, serde_json::Value>,
    msg: &BlockSyncMessage,
) {
    match msg {
        BlockSyncMessage::Status(status) => {
            bs_insert_height(row, bs_trace::RANGE_START, status.servable_low);
            bs_insert_height(row, bs_trace::HEIGHT, status.servable_high);
        }
        BlockSyncMessage::Block(block) => {
            bs_insert_hash(row, bs_trace::HASH, block.hash());
            if let Some(height) = block.coinbase_height() {
                bs_insert_height(row, bs_trace::HEIGHT, height);
            }
        }
        BlockSyncMessage::BlocksDone {
            start_height,
            returned,
        } => {
            bs_insert_height(row, bs_trace::RANGE_START, *start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(*returned));
        }
        BlockSyncMessage::RangeUnavailable {
            start_height,
            count,
        }
        | BlockSyncMessage::GetBlocks {
            start_height,
            count,
        } => {
            bs_insert_height(row, bs_trace::RANGE_START, *start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(*count));
        }
    }
}

fn block_sync_message_label(msg: &BlockSyncMessage) -> &'static str {
    match msg {
        BlockSyncMessage::Status(_) => "status",
        BlockSyncMessage::Block(_) => "block",
        BlockSyncMessage::BlocksDone { .. } => "blocks_done",
        BlockSyncMessage::RangeUnavailable { .. } => "range_unavailable",
        BlockSyncMessage::GetBlocks { .. } => "get_blocks",
    }
}

fn block_misbehavior_label(reason: BlockSyncMisbehavior) -> &'static str {
    match reason {
        BlockSyncMisbehavior::MalformedMessage => "malformed_message",
        BlockSyncMisbehavior::UnsolicitedBlock => "unsolicited_block",
        BlockSyncMisbehavior::GetBlocksTooLong => "get_blocks_too_long",
        BlockSyncMisbehavior::GetBlocksSpam => "get_blocks_spam",
        BlockSyncMisbehavior::InvalidBlock => "invalid_block",
        BlockSyncMisbehavior::SizeMismatch => "size_mismatch",
        BlockSyncMisbehavior::InvalidStatus => "invalid_status",
        BlockSyncMisbehavior::UnsolicitedDone => "unsolicited_done",
        BlockSyncMisbehavior::RangeUnavailable => "range_unavailable",
        BlockSyncMisbehavior::StatusSpam => "status_spam",
    }
}

fn bs_insert_str(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    value: &str,
) {
    row.insert(key.to_string(), serde_json::Value::from(value.to_string()));
}

fn tolerated_bytes(reserved_bytes: u64, tolerance_percent: u32) -> u64 {
    reserved_bytes.saturating_mul(u64::from(tolerance_percent.max(100))) / 100
}

fn block_sync_misbehavior_is_soft(reason: BlockSyncMisbehavior) -> bool {
    matches!(
        reason,
        BlockSyncMisbehavior::SizeMismatch
            | BlockSyncMisbehavior::RangeUnavailable
            | BlockSyncMisbehavior::GetBlocksSpam
    )
}
