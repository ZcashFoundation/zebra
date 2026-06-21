use super::{
    config::*, events::*, peer_registry::*, sequencer::*, sequencer_task::*, state::*, wire::*, *,
};
use crate::zakura::{
    FrontierChange, FrontierUpdate, ServiceAdmissionDecision, ServicePeerDirection,
    ServicePeerSnapshot, ZakuraBlockSyncCandidateState,
};
use iroh::NodeId;

/// Upper bound on how long the reactor will wait to enqueue a data-plane action
/// before abandoning it. The bounded `actions` channel is normally drained by
/// the action driver almost immediately; this deadline only trips when that
/// driver is genuinely stalled on backend/verifier work, and it keeps a stalled
/// driver from wedging the reactor's control plane — peer-lifecycle draining,
/// request timeouts, and above all misbehavior disconnects.
const ACTION_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Spare action-channel slots kept above `submitted_apply_limit` for queries and
/// misbehavior actions. The channel is sized so a full checkpoint window of
/// `SubmitBlock`s plus this pool fit without the reactor ever blocking on a
/// required-action `send().await`.
const BS_ACTION_SPARE_POOL: usize = 128;

/// Bound on the shared routine→reactor channel (status-advertise / serve /
/// re-query / serving-misbehavior). Sized generously so a transient burst of
/// per-peer events never makes a routine's `try_send` drop a serving/status
/// request; the routine never blocks on it (the only blocking routine send is the
/// Sequencer `AcceptBody`), so a full channel just defers an idempotent ping.
const ROUTINE_TO_REACTOR_DEPTH: usize = 1024;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FloorGapDiagnostics {
    height: block::Height,
    state: &'static str,
    servable_peers: usize,
    available_peers: usize,
    outstanding_peers: usize,
    oldest_outstanding_ms: Option<u64>,
    next_deadline_ms: Option<u64>,
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
    let events_keepalive = events_tx.clone();
    let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
    // Size the action channel so the reactor can dispatch a full checkpoint
    // window of `SubmitBlock`s (`submitted_apply_limit`) plus the query/misbehavior
    // spare pool without blocking on a required-action `send().await`. The byte
    // budget — not this channel — bounds in-flight body memory, and `SubmitBlock`
    // only carries an `Arc<Block>` already accounted in `applying`, so the larger
    // channel costs negligible memory while removing a head-of-line stall that
    // throttled body intake behind commit submission.
    let actions_capacity = startup
        .config
        .submitted_apply_limit()
        .saturating_add(BS_ACTION_SPARE_POOL);
    let (actions_tx, actions_rx) = mpsc::channel(actions_capacity);
    let (peers_tx, peers_rx) = watch::channel(state.peer_snapshot(startup.config.peer_limits));
    let (status_tx, status_rx) = watch::channel(state.last_advertised_status);
    let (candidates_tx, candidates_rx) = watch::channel(ZakuraBlockSyncCandidateState::default());

    // The Sequencer (commit pipeline) and the committed-throughput meter move out
    // of the reactor onto their own serial task (S3b). The reactor forwards every
    // Sequencer-mutating event over a bounded ordered input channel and learns
    // committed progress back over a non-blocking `watch`.
    let sequencer = Sequencer::new(
        startup.frontiers.verified_block_tip,
        startup.config.submitted_apply_limit(),
    );
    let committed_throughput = ThroughputMeter::new(Instant::now());
    // Bound the input channel at the submission window so a slow verifier
    // backpressures the reactor's forwarding without an unbounded queue.
    let (sequencer_input_tx, sequencer_input_rx) =
        mpsc::channel(startup.config.submitted_apply_limit().max(1));
    let (sequencer_view_tx, sequencer_view_rx) = watch::channel(initial_view(startup.frontiers));

    let sequencer_task = SequencerTask::new(
        sequencer,
        state.budget.clone(),
        state.work.clone(),
        actions_tx.clone(),
        committed_throughput,
        startup.frontiers,
        sequencer_input_rx,
        sequencer_view_tx,
        ACTION_SEND_TIMEOUT,
        startup.trace.clone(),
    );
    tokio::spawn(sequencer_task.run());

    // S4: the shared per-peer fact table read by the producer / candidate / trace
    // and written by the routines (servable/caps/outstanding) and the reactor
    // (admission/teardown entry insert/remove).
    let registry = Arc::new(PeerRegistry::new());
    // The shared routine→reactor channel: every per-peer pipe-routine forwards its
    // serving / status-advertise / re-query / serving-misbehavior concerns here.
    let (routine_to_reactor_tx, routine_to_reactor_rx) = mpsc::channel(ROUTINE_TO_REACTOR_DEPTH);
    let routine_to_reactor_keepalive = routine_to_reactor_tx.clone();

    // The shared download primitives every pipe-routine is wired with at spawn
    // (`service::add_peer`), carried through the handle.
    let routine_wiring = RoutineWiring {
        config: startup.config.clone(),
        budget: state.budget.clone(),
        work: state.work.clone(),
        registry: registry.clone(),
        received_throughput: state.received_throughput.clone(),
        sequencer_input: sequencer_input_tx.clone(),
        actions: actions_tx.clone(),
        routine_to_reactor: routine_to_reactor_tx,
        view: sequencer_view_rx.clone(),
        trace: startup.trace.clone(),
    };

    let handle = BlockSyncHandle {
        events: events_tx,
        lifecycle: lifecycle_tx,
        peers: peers_rx,
        status: status_rx,
        candidates: candidates_rx,
        routine_wiring: Some(routine_wiring),
    };
    let reactor = BlockSyncReactor {
        verified_block_tip: startup.frontiers.verified_block_tip,
        committed_floor: startup.frontiers.verified_block_tip,
        last_reset_epoch: 0,
        last_reaction_epoch: 0,
        last_view: initial_view(startup.frontiers),
        startup,
        state,
        registry,
        events: events_rx,
        _events_keepalive: events_keepalive,
        lifecycle: lifecycle_rx,
        actions: actions_tx,
        routine_to_reactor: routine_to_reactor_rx,
        _routine_to_reactor_keepalive: routine_to_reactor_keepalive,
        peers: peers_tx,
        status: status_tx,
        candidates: candidates_tx,
        sequencer_input: sequencer_input_tx,
        sequencer_view: sequencer_view_rx,
    };
    let task = tokio::spawn(reactor.run());

    (handle, actions_rx, task)
}

#[derive(Debug)]
pub(super) struct BlockSyncReactor {
    startup: BlockSyncStartup,
    state: BlockSyncState,
    /// Shared per-peer fact table (S4): servable/caps/outstanding written by the
    /// per-peer pipe-routines; read by producer/candidate/trace. The reactor owns
    /// only entry insert (admission) / remove (teardown).
    registry: Arc<PeerRegistry>,
    events: mpsc::Receiver<BlockSyncEvent>,
    /// A keep-alive sender clone for the bounded driver-event channel so the
    /// receiver never resolves to `None` while the reactor lives. S4 dropped the
    /// service's stored `events` sender (it was only used by the deleted
    /// `deliver_frame` pipe path), so without this the channel would close as soon
    /// as a consumer moved (not cloned) the handle. The reactor never sends on it.
    _events_keepalive: mpsc::Sender<BlockSyncEvent>,
    lifecycle: mpsc::UnboundedReceiver<BlockSyncEvent>,
    actions: mpsc::Sender<BlockSyncAction>,
    /// Shared routine→reactor channel: serving (`ServeGetBlocks`), status
    /// advertisement (`StatusReceived`), the producer re-query ping
    /// (`RequeryNeeded`), and serving-side misbehavior (`Misbehavior`).
    routine_to_reactor: mpsc::Receiver<RoutineToReactor>,
    /// A keep-alive sender clone so the receiver never resolves to `None` while
    /// the reactor lives, even before any peer connects or after all disconnect.
    /// The reactor never sends on it; only shutdown (dropping the reactor) closes
    /// the channel.
    _routine_to_reactor_keepalive: mpsc::Sender<RoutineToReactor>,
    peers: watch::Sender<ServicePeerSnapshot>,
    status: watch::Sender<BlockSyncStatus>,
    candidates: watch::Sender<ZakuraBlockSyncCandidateState>,
    /// Bounded ordered channel to the Sequencer task: every Sequencer-mutating
    /// event the reactor demuxes, forwarded in event order.
    sequencer_input: mpsc::Sender<SequencerInput>,
    /// Latest-wins committed view published by the Sequencer task.
    sequencer_view: watch::Receiver<SequencerView>,
    /// Reactor-side mirror of the Sequencer's verified tip (it no longer lives
    /// in `state`). Updated from the committed view; initialized from startup.
    verified_block_tip: block::Height,
    /// Reactor-side mirror of the Sequencer's body-download floor. Used ONLY for
    /// the producer query lower bound, candidate prune, and stale-prefix trim —
    /// never as a fetch decision (design doc §7.8).
    committed_floor: block::Height,
    /// Last `reset_epoch` the reactor reacted to, so it can tell an advance from
    /// a destructive reset.
    last_reset_epoch: u64,
    /// Last `reaction_epoch` the reactor reacted to. The heavy serving/producer
    /// reaction runs only when this advances (i.e. the view reflects a processed
    /// frontier/reset/apply input, not a pure body buffer/submit).
    last_reaction_epoch: u64,
    /// Latest view snapshot, kept so the periodic trace tick can read the
    /// (remote) Sequencer's reorder/applying/throughput counters.
    last_view: SequencerView,
}

impl BlockSyncReactor {
    async fn run(mut self) {
        let mut header_tip = self.startup.header_tip.clone();
        let mut header_tip_open = header_tip.is_some();
        let mut frontier_updates = self.startup.frontier_updates.clone();
        let mut frontier_updates_open = frontier_updates.is_some();
        // Metrics/trace snapshot cadence only. Per-peer request timeouts are owned
        // by the routines (each sleeps to its own earliest deadline), so this timer
        // no longer drives any timeout; it reuses `request_timeout` purely as a
        // reasonable periodic refresh interval.
        let mut metrics_ticks = time::interval(self.startup.config.request_timeout);
        let mut status_ticks = time::interval(
            self.startup
                .config
                .status_refresh_interval
                .max(Duration::from_millis(1)),
        );

        self.query_needed_blocks().await;
        self.publish_metrics();
        self.refresh_throughput();
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
                changed = self.sequencer_view.changed() => {
                    match changed {
                        Ok(()) => {
                            let view = *self.sequencer_view.borrow_and_update();
                            self.on_sequencer_view_changed(view).await;
                            self.publish_metrics();
                            // Snapshot the committed state on every view change, not
                            // only on the periodic tick. Commit progress (including
                            // the final `applying -> 0` settle near the tip) arrives
                            // as a view change; without a snapshot here the trace's
                            // last `commit_state` row lags the live metric, and the
                            // e2e oracle reads a stale `applying > 0` "leak" after the
                            // node has actually settled (the live metric reads 0).
                            self.trace_sync_state();
                        }
                        Err(_) => break,
                    }
                }
                message = self.routine_to_reactor.recv() => {
                    // The routine→reactor channel is held alive by the wiring on
                    // the handle (cloned into every spawned pipe-routine and on the
                    // service); `recv()` only resolves to `None` once every sender
                    // is dropped (shutdown). The reactor handles serving / status
                    // advertisement / re-query / serving-misbehavior here.
                    match message {
                        Some(message) => self.handle_routine_message(message).await,
                        None => break,
                    }
                }
                _ = metrics_ticks.tick() => {
                    self.publish_metrics();
                    self.refresh_throughput();
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
        let needed = &self.state.needed_heights;
        let mut admitted_node_ids: Vec<_> = self
            .registry
            .candidate_snapshot()
            .into_iter()
            .filter_map(|(peer_id, received_status, servable_low, servable_high)| {
                if has_body_gaps {
                    let can_serve_any = received_status
                        && needed
                            .iter()
                            .any(|height| servable_low <= *height && *height <= servable_high);
                    if !can_serve_any {
                        return None;
                    }
                }
                node_id_from_block_peer_id(&peer_id)
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
        let decision = self.admission_decision_for(&peer, direction);
        if decision != ServiceAdmissionDecision::Admit {
            // Reject: cancel the session (which also cancels the already-spawned
            // pipe-routine, whose `Drop` returns any taken work) and drop the
            // routine's registry entry so a parked peer leaves no stale facts.
            self.state.parked_peers.insert(peer.clone());
            session.cancel_token().cancel();
            self.registry.remove(&peer);
            self.publish_peer_snapshot();
            self.publish_candidate_state();
            return;
        }

        self.state.parked_peers.remove(&peer);
        // S4 inverted flow: the per-peer pipe-routine was already spawned by
        // `service::add_peer` (the pipe spawn point), wired with the shared
        // primitives and its registry generation. The reactor keeps only a thin
        // serving handle (session + serving meters) — it neither spawns the
        // routine nor holds a per-peer inbound channel.
        let mut peer_state = PeerBlockState::new(session, &self.startup.config);
        // Consume the status-advertisement refresh allowance: the connect Status
        // below counts as this peer's first advertisement, so the next periodic
        // refresh must wait a full interval before re-sending (matches the pre-S4
        // `unsolicited.mark_taken` at connect).
        peer_state.refresh_meter.mark_taken(Instant::now());
        self.state.peers.insert(peer.clone(), peer_state);

        self.trace_peer_connected(&peer, direction);
        self.publish_peer_snapshot();
        self.publish_candidate_state();
        self.send_status(&peer, "peer_connected").await;
        // The routine fills its own slots; it begins want-work as soon as it has
        // a status and work.
    }

    fn handle_peer_disconnected(&mut self, peer: ZakuraPeerId) {
        // The pipe-routine cancels on the session token (transport disconnect);
        // its `Drop` guard returns its unreceived outstanding heights to
        // `work.pending` and releases their budget. The reactor only drops its
        // thin serving handle and the registry entry.
        if self.state.peers.remove(&peer).is_some() {
            self.trace_peer_disconnected(&peer, self.registry_received_status(&peer));
        }
        self.registry.remove(&peer);
        self.state.parked_peers.remove(&peer);
        self.publish_peer_snapshot();
        self.publish_candidate_state();
    }

    fn registry_received_status(&self, peer: &ZakuraPeerId) -> bool {
        self.registry.has_received_status(peer)
    }

    async fn handle_header_tip_changed(&mut self, height: block::Height, hash: block::Hash) {
        self.state.best_header_tip = height;
        self.state.best_header_hash = hash;
        self.query_needed_blocks().await;
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
                if frontier.verified_body.height > self.verified_block_tip {
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
        // Per-peer request timeouts are reclaimed by the routines themselves (each
        // sleeps to its earliest deadline); the reactor no longer sweeps here.
    }

    async fn handle_state_frontiers_changed(&mut self, frontiers: BlockSyncFrontiers) {
        // Reactor-owned prep: fold the finalized height forward (the Sequencer
        // task folds it too, but the reactor mirror must not regress in the
        // window before the view comes back), then forward the Sequencer-side
        // advance. The stale-frontier guard reads the verified-tip mirror; a
        // genuinely stale update is dropped here so we do not forward a no-op.
        self.state.finalized_height = self.state.finalized_height.max(frontiers.finalized_height);
        if frontiers.verified_block_tip < self.verified_block_tip {
            tracing::debug!(
                current = ?self.verified_block_tip,
                stale = ?frontiers.verified_block_tip,
                "ignoring stale Zakura block-sync frontier update"
            );
            return;
        }
        // The `frontiers_changed` trace now fires from the view reaction, where the
        // task has reported whether the tip actually moved (a same-tip snapshot is a
        // no-op there, matching the original's `if advance.changed` gating).
        let _ = self
            .sequencer_input
            .send(SequencerInput::FrontierAdvance {
                frontiers,
                release_applied: true,
            })
            .await;
    }

    /// Drop now-committed heights from the published `needed_heights` set when the
    /// floor advances. The producer (`handle_needed_blocks`) only ever *grows*
    /// `needed_heights`; this prunes the heights the floor passed so the candidate
    /// gap clears promptly without waiting for the next `NeededBlocks` snapshot.
    /// Reads the `committed_floor` mirror (the Sequencer's floor now lives on the
    /// task); this is a GC/candidate use, never a fetch throttle (§7.8).
    fn prune_needed_below_floor(&mut self) {
        let floor = self.committed_floor;
        let before = self.state.needed_heights.len();
        self.state.needed_heights.retain(|height| *height > floor);
        if self.state.needed_heights.len() != before {
            self.publish_candidate_state();
        }
    }

    async fn handle_chain_tip_reset(
        &mut self,
        frontiers: BlockSyncFrontiers,
        preserve_active_successors: bool,
    ) {
        // Reactor-owned prep: precompute the two peer-outstanding-derived halves
        // of the reset decision (the reactor owns peer state; the task ORs them
        // with its Sequencer-internal predicates). The `verified_tip()`/`floor()`
        // reads in the original decision are NOT recomputed here: the task owns
        // them and makes the destructive-vs-growth call against its own
        // authoritative copy.
        let tip = frontiers.verified_block_tip;
        // S4: peer `outstanding` lives in the routines, mirrored into the registry
        // (per-peer *unreceived* in-flight heights). The reactor reads it from the
        // registry to precompute the two peer-derived halves of the reset
        // decision. Received-and-buffered heights are caught by the Sequencer's own
        // reorder/applying predicates, so reading only unreceived heights here is a
        // benign (correct) narrowing of the original `expected_hashes` scan.
        let peer_has_successor_after = next_height(tip)
            .map(|next| self.registry.any_outstanding_at_or_above(next))
            .unwrap_or(false);
        let peer_outstanding_conflicts_at_tip = self
            .registry
            .any_outstanding_conflicts_at(tip, frontiers.verified_block_hash);

        // The `chain_tip_reset` trace now fires from the view reaction on a
        // `reset_epoch` bump (the task's destructive path), so it is not emitted for
        // a growth-classified reset — matching the original.
        let _ = self
            .sequencer_input
            .send(SequencerInput::FrontierReset {
                frontiers,
                preserve_active_successors,
                peer_has_successor_after,
                peer_outstanding_conflicts_at_tip,
            })
            .await;
    }

    /// React to the latest committed view from the Sequencer task: update the
    /// reactor's committed mirrors, then run the serving/peer/candidate/producer
    /// half that used to follow the inline Sequencer mutation
    /// (status refresh, candidate prune, drop-outstanding, re-query, re-schedule).
    async fn on_sequencer_view_changed(&mut self, view: SequencerView) {
        // Always update the committed mirrors so the producer lower bound,
        // candidate prune, and trace read the latest floor/tip — even on a
        // view change that only reflects buffering/submission. The mirrors are
        // read-only control inputs (§7.8); updating them is cheap and idempotent.
        let reset_advanced = view.reset_epoch != self.last_reset_epoch;
        let reaction_advanced = view.reaction_epoch != self.last_reaction_epoch;
        let old_serving_tip = (self.state.servable_high, self.state.servable_hash);
        let tip_advanced = view.verified_tip > self.verified_block_tip;

        self.last_view = view;
        self.state.finalized_height = self.state.finalized_height.max(view.finalized);
        self.verified_block_tip = view.verified_tip;
        self.committed_floor = view.floor;
        self.state.verified_block_hash = view.verified_hash;
        self.state.servable_high = view.verified_tip;
        self.state.servable_hash = view.verified_hash;

        // The heavy serving/peer/candidate/producer reaction (drop-outstanding,
        // prune, status, query, schedule) ran in the single-task version for a
        // frontier advance, reset, or apply-finished — never for a pure body
        // buffer/submit, which only reschedules the forwarding peer (the reactor
        // already did that after forwarding `AcceptBody`). The `reaction_epoch`
        // advances exactly for those inputs.
        if !reaction_advanced {
            return;
        }
        self.last_reaction_epoch = view.reaction_epoch;

        if reset_advanced {
            // A destructive reset (S4: reset = in-place clear in each routine). The
            // Sequencer already pinned its floor/tip and `work.reset_above`'d the
            // dropped successor heights, then bumped `reset_epoch`. Each per-peer
            // pipe-routine watches the same `view`: on the `reset_epoch` bump it
            // clears its own outstanding in place (returns unreceived heights to
            // `work.pending`, releases their budget) and re-fans — no task teardown,
            // so the transport is never torn down and there is no respawn
            // double-claim race. The reactor only re-runs the producer/serving tail
            // below; the routines self-reset off the view.
            self.last_reset_epoch = view.reset_epoch;
            self.trace_chain_tip_reset(view.verified_tip);
        } else if tip_advanced {
            // A non-destructive frontier advance does NOT respawn and does NOT
            // proactively drop outstanding through the tip: a still-open request
            // for a now-committed height releases its budget on delivery (Sequencer
            // `Redundant`) or on its own timeout, so there is no leak, only a
            // slightly later release (§6 S4 "reset = respawn"). Trace the change
            // only when the tip actually moved.
            self.trace_frontiers_changed(view.verified_tip);
        }
        self.prune_needed_below_floor();

        self.queue_status_refresh_if_changed(old_serving_tip);
        self.flush_status_refresh().await;
        self.query_needed_blocks().await;
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
        // Producer filter (S3b substitution): the Sequencer's reorder/applying/
        // submitted predicates are no longer reactor-local. They are replaced by
        // the structural invariant "held-or-outstanding ⟺ `work.in_flight`":
        // every buffered/applying/submitted/outstanding height was taken into
        // `in_flight` at issuance and leaves only via `advance_floor` (committed)
        // or `reset_above` (reset). So a height above the committed floor that is
        // not in `in_flight` is genuinely missing and re-queuable; one that is
        // in `in_flight` is already claimed and must not be re-issued. The
        // `committed_floor` mirror is the producer's lower bound only (§7.8).
        //
        // `!has_outstanding_request` is kept (the registry's per-peer outstanding):
        // the `in_flight ⟺ outstanding` half of the invariant breaks transiently
        // when a reject/timeout rollback `reset_above`s a still-downloading
        // successor out of `in_flight` while its peer request is still
        // outstanding-but-unreceived. Without this clause the producer would
        // re-queue that height and issue a duplicate concurrent fetch (the original
        // filter excluded it the same way; the stale outstanding clears on its own
        // timeout). The hash is checked so a reanchor (different hash) still
        // re-queues. The registry's outstanding is routine-owned, so this clause is
        // now backed by per-peer state independent of `work.in_flight`.
        let blocks: Vec<_> = blocks
            .into_iter()
            .filter(|block| {
                block.height > self.committed_floor
                    && !self.state.work.in_flight_contains(block.height)
                    && !self
                        .registry
                        .has_outstanding_request(block.height, block.hash)
            })
            .collect();

        self.state.needed_heights = blocks.iter().map(|block| block.height).collect();
        self.state.needed_heights.sort_unstable();
        self.state.needed_heights.dedup();

        // The WorkQueue producer is additive and idempotent: `extend` inserts only
        // heights above the floor that are not already pending or in flight, so a
        // buffered/in-flight height is never re-queued and a stale snapshot cannot
        // duplicate work. Heights below the floor are GC'd by `advance_floor`;
        // heights above a reset target by `reset_above`. The per-peer routines pick
        // the new work up via `work.subscribe_available()` (the §7.3 wake), so the
        // reactor no longer schedules here. Stale-hash pruning of an *outstanding*
        // request is now owned by the routine (and reset = in-place clear) rather
        // than the reactor's old `drop_ranges_not_in_needed`.
        //
        // `extend` runs BEFORE the candidate publish so the candidate watch update
        // is a reliable "work is now in `pending`" signal: a routine that sees the
        // candidate set grow (or any observer of the candidate watch) can rely on
        // the matching heights already being takeable, with no extend-vs-observe
        // race.
        let count = self.state.work.extend(
            blocks
                .into_iter()
                .map(|block| (block.height, block.hash, block.size)),
        );
        self.trace_work_extended(count);
        self.publish_candidate_state();
    }

    /// Header tip minus verified body tip, emitted as the `body_lag` trace field
    /// only. Downloads gate on byte budget + per-peer slots — never on this lag
    /// (no near-tip pause); see the design doc §7.8.
    fn body_lag(&self) -> u32 {
        self.state
            .best_header_tip
            .0
            .saturating_sub(self.verified_block_tip.0)
    }

    /// Handle one shared routine→reactor message (S4 inverted flow). The per-peer
    /// pipe-routines forward only the concerns that need reactor-global state:
    /// serving, status advertisement, the producer re-query, and serving-side
    /// misbehavior.
    async fn handle_routine_message(&mut self, message: RoutineToReactor) {
        match message {
            RoutineToReactor::StatusReceived { peer, send_reply } => {
                self.handle_status_received(peer, send_reply).await;
            }
            RoutineToReactor::ServeGetBlocks {
                peer,
                start_height,
                count,
            } => {
                if self.state.parked_peers.contains(&peer) {
                    return;
                }
                self.handle_get_blocks(peer, start_height, count).await;
            }
            RoutineToReactor::RequeryNeeded => {
                self.query_needed_blocks().await;
            }
            RoutineToReactor::Misbehavior { peer, reason } => {
                self.report_misbehavior(peer, reason).await;
            }
        }
    }

    /// A routine applied a peer's `Status` (servable/caps already written to the
    /// registry by the routine, generation-gated). The reactor advertises our
    /// `Status` reply if the routine's rate meter allowed it and republishes the
    /// candidate set.
    async fn handle_status_received(&mut self, peer: ZakuraPeerId, send_reply: bool) {
        if !self.state.peers.contains_key(&peer) {
            return;
        }
        self.publish_candidate_state();
        if send_reply {
            self.send_status(&peer, "status_reply").await;
        }
    }

    async fn handle_get_blocks(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        count: u32,
    ) {
        let local_inflight_cap = self.startup.config.advertised_max_inflight_requests();
        if !self.state.peers.contains_key(&peer) {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksSpam)
                .await;
            return;
        }

        // `received_status` is now a registry fact (written reactor-side on
        // `Status`); the serving slots stay on the reactor's thin peer handle.
        if !self.registry_received_status(&peer) {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksSpam)
                .await;
            return;
        }

        if count == 0 {
            self.report_misbehavior(peer, BlockSyncMisbehavior::GetBlocksTooLong)
                .await;
            return;
        }

        let started_serving = self.state.peers.get_mut(&peer).is_some_and(|peer_state| {
            peer_state.try_start_serving_blocks(local_inflight_cap, start_height)
        });
        if !started_serving {
            let unavailable_count = count.min(inbound_get_blocks_count_limit(&self.startup.config));
            self.send_range_unavailable(&peer, start_height, unavailable_count);
            return;
        }

        let requested_count = self.clamp_served_block_count(start_height, count);
        if requested_count == 0 {
            let unavailable_count = count.min(inbound_get_blocks_count_limit(&self.startup.config));
            self.send_range_unavailable(&peer, start_height, unavailable_count);
            self.finish_serving_blocks(&peer, start_height);
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
            self.finish_serving_blocks(&peer, start_height);
        }
    }

    async fn handle_block_range_response_ready(
        &mut self,
        peer: ZakuraPeerId,
        start_height: block::Height,
        requested_count: u32,
        blocks: Vec<(block::Height, Arc<block::Block>, usize)>,
    ) {
        let prepare_elapsed = self.serving_blocks_elapsed(&peer, start_height);
        let send_started = Instant::now();
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
        let total_elapsed = self.finish_serving_blocks(&peer, start_height);
        self.trace_range_response_sent(
            &peer,
            start_height,
            requested_count,
            sent_blocks,
            sent_bytes,
            reason,
            prepare_elapsed,
            send_started.elapsed(),
            total_elapsed,
        );
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
        let elapsed = self.finish_serving_blocks(&peer, start_height);
        self.trace_range_response_sent(
            &peer,
            start_height,
            requested_count,
            returned_count,
            0,
            "driver_finished",
            elapsed,
            Duration::ZERO,
            elapsed,
        );
    }

    async fn handle_block_apply_finished(
        &mut self,
        token: BlockApplyToken,
        height: block::Height,
        hash: block::Hash,
        result: BlockApplyResult,
        local_frontier: Option<BlockSyncFrontiers>,
    ) {
        // The whole commit-pipeline body (token validate, embedded local-frontier
        // advance, applying removal, budget release, throughput record, rollback +
        // misbehavior, drain + submit) runs on the Sequencer task. The reactor
        // forwards the completion and reacts to the resulting committed view
        // (serving/status/query/schedule) on the `view` arm.
        self.trace_apply_finished(height, token, result, self.state.budget.reserved());
        let _ = self
            .sequencer_input
            .send(SequencerInput::ApplyFinished {
                token,
                height,
                hash,
                result,
                local_frontier,
            })
            .await;
    }

    fn serving_blocks_elapsed(
        &self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
    ) -> Option<Duration> {
        self.state
            .peers
            .get(peer)
            .and_then(|peer_state| peer_state.serving_blocks_elapsed(start_height))
    }

    fn finish_serving_blocks(
        &mut self,
        peer: &ZakuraPeerId,
        start_height: block::Height,
    ) -> Option<Duration> {
        self.state
            .peers
            .get_mut(peer)
            .and_then(|peer_state| peer_state.finish_serving_blocks(start_height))
    }

    async fn query_needed_blocks(&mut self) -> bool {
        if !self.startup.state_queries_enabled {
            return false;
        }
        if self.local_body_work_blocks() >= self.refill_low_water_blocks() {
            return true;
        }
        let _ = self
            .dispatch_action(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: self.committed_floor,
                best_header_tip: self.state.best_header_tip,
            })
            .await;
        true
    }

    fn local_body_work_blocks(&self) -> usize {
        // The unreceived in-flight heights now live in the routines, mirrored into
        // the registry's per-peer outstanding set (per-request granularity: each
        // entry is one still-unreceived requested height). `total_unreceived` sums
        // them — the same count the old per-peer `expected_hashes − received`
        // produced.
        let outstanding = self.registry.total_unreceived();

        // Count only the download pipeline (pending WorkQueue heights + the
        // unreceived heights of in-flight requests) against the refill low-water
        // mark, never the commit pipeline (`reorder` + `applying`). Downloads are
        // bounded by the in-flight byte budget and per-peer slots, not by how fast
        // commit/verify drains. Including reorder/applying here would pace
        // downloads to commit speed: a slow commit lets those buffers grow, the
        // low-water gate stops refilling, and `outstanding` collapses. The byte
        // budget already bounds memory because reorder/applying hold their
        // reservation until apply-finish, so downloads may legitimately run far
        // ahead of commit up to that budget.
        self.state.work.pending_len().saturating_add(outstanding)
    }

    fn refill_low_water_blocks(&self) -> usize {
        let status_peers = self.registry.peers_with_status().max(1);
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

    async fn send_status(&self, peer: &ZakuraPeerId, reason: &'static str) {
        let Some(peer_state) = self.state.peers.get(peer) else {
            return;
        };
        let status = self.local_status();
        let session = peer_state.session.clone();
        let msg = BlockSyncMessage::Status(status);
        let started = Instant::now();
        if let Err(error) = session.try_send_status(status) {
            tracing::debug!(?peer, ?error, "failed to queue Zakura block-sync Status");
            self.trace_status_send_failed(peer, reason);
            self.trace_message_sent(peer, &msg, "error", started.elapsed());
            session.cancel_token().cancel();
            return;
        }
        self.trace_message_sent(peer, &msg, "queued", started.elapsed());
        self.trace_status_sent(peer, reason, status);
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
        let msg = BlockSyncMessage::Block(block.clone());
        let started = Instant::now();
        match time::timeout(ACTION_SEND_TIMEOUT, session.send_block(block)).await {
            Ok(Ok(())) => {
                metrics::counter!("sync.block.body.served").increment(1);
                self.trace_message_sent(peer, &msg, "queued", started.elapsed());
                true
            }
            Ok(Err(error)) => {
                tracing::debug!(?peer, ?error, "failed to queue Zakura block-sync Block");
                self.trace_message_sent(peer, &msg, "error", started.elapsed());
                session.cancel_token().cancel();
                false
            }
            Err(_) => {
                metrics::counter!("sync.block.body.serve_timeout").increment(1);
                tracing::debug!(?peer, "timed out queueing Zakura block-sync Block");
                self.trace_message_sent(peer, &msg, "timeout", started.elapsed());
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
        let msg = BlockSyncMessage::BlocksDone {
            start_height,
            returned,
        };
        let started = Instant::now();
        match time::timeout(
            ACTION_SEND_TIMEOUT,
            session.send_blocks_done(start_height, returned),
        )
        .await
        {
            Ok(Ok(())) => self.trace_message_sent(peer, &msg, "queued", started.elapsed()),
            Ok(Err(error)) => {
                tracing::debug!(
                    ?peer,
                    ?error,
                    "failed to queue Zakura block-sync BlocksDone"
                );
                self.trace_message_sent(peer, &msg, "error", started.elapsed());
                session.cancel_token().cancel();
            }
            Err(_) => {
                metrics::counter!("sync.block.done.serve_timeout").increment(1);
                tracing::debug!(?peer, "timed out queueing Zakura block-sync BlocksDone");
                self.trace_message_sent(peer, &msg, "timeout", started.elapsed());
                session.cancel_token().cancel();
            }
        }
    }

    fn send_range_unavailable(&self, peer: &ZakuraPeerId, start_height: block::Height, count: u32) {
        let count = count.max(1);
        let Some(peer_state) = self.state.peers.get(peer) else {
            return;
        };
        let msg = BlockSyncMessage::RangeUnavailable {
            start_height,
            count,
        };
        let started = Instant::now();
        if let Err(error) = peer_state
            .session
            .try_send_range_unavailable(start_height, count)
        {
            tracing::debug!(
                ?peer,
                ?error,
                "failed to queue Zakura block-sync RangeUnavailable"
            );
            self.trace_message_sent(peer, &msg, "error", started.elapsed());
            peer_state.session.cancel_token().cancel();
        } else {
            self.trace_message_sent(peer, &msg, "queued", started.elapsed());
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
        let msg = BlockSyncMessage::RangeUnavailable {
            start_height,
            count,
        };
        let started = Instant::now();
        match time::timeout(
            ACTION_SEND_TIMEOUT,
            session.send_range_unavailable(start_height, count),
        )
        .await
        {
            Ok(Ok(())) => self.trace_message_sent(peer, &msg, "queued", started.elapsed()),
            Ok(Err(error)) => {
                tracing::debug!(
                    ?peer,
                    ?error,
                    "failed to queue Zakura block-sync RangeUnavailable"
                );
                self.trace_message_sent(peer, &msg, "error", started.elapsed());
                session.cancel_token().cancel();
            }
            Err(_) => {
                metrics::counter!("sync.block.unavailable.serve_timeout").increment(1);
                tracing::debug!(
                    ?peer,
                    "timed out queueing Zakura block-sync RangeUnavailable"
                );
                self.trace_message_sent(peer, &msg, "timeout", started.elapsed());
                session.cancel_token().cancel();
            }
        }
    }

    async fn flush_status_refresh(&mut self) {
        // `received_status` is a registry fact now; snapshot which peers have not
        // acknowledged our status so the retry filter below can read it without
        // re-locking per peer.
        let unready: HashSet<ZakuraPeerId> = self
            .registry
            .candidate_snapshot()
            .into_iter()
            .filter_map(|(peer, received_status, _, _)| (!received_status).then_some(peer))
            .collect();
        let has_unready_peers = !unready.is_empty();
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
                    peer.refresh_meter.mark_taken(now);
                    Some(peer_id.clone())
                } else if unready.contains(peer_id) && peer.refresh_meter.try_take(now) {
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
    /// Recompute the cached download/commit throughput rates for the next trace
    /// snapshot. Called on the trace tick (not the body hot path) so the rate is
    /// measured over the inter-tick interval.
    fn refresh_throughput(&mut self) {
        let now = Instant::now();
        // The received (download) meter is shared with the routines (they record
        // on receipt); only the reactor samples it. The committed (commit) rate is
        // sampled by the Sequencer task and read from the latest view snapshot.
        if let Ok(mut meter) = self.state.received_throughput.lock() {
            meter.sample(now);
        }
    }

    fn trace_sync_state(&self) {
        if !self.startup.trace.is_enabled() {
            return;
        }
        let floor_gap = self.floor_gap_diagnostics(Instant::now());
        // The per-peer download window now lives in the routines, mirrored into the
        // registry slot diagnostics; the periodic row sums them. `outstanding` here
        // is the registry's outstanding-request count across peers.
        let slots = self.registry.slot_summary();
        let outstanding = slots.outstanding_requests;
        let slot_capacity = slots.capacity;
        let slot_effective_window = slots.effective_window;
        let slot_available = slots.available;
        let slot_timeout_recovery = slots.timeout_recovery;
        let slot_saturated_peers = slots.saturated_peers;
        let counts = self.registry.direction_status_counts();
        let inbound_peers = counts.inbound;
        let outbound_peers = counts.outbound;
        let inbound_peers_with_status = counts.inbound_with_status;
        let outbound_peers_with_status = counts.outbound_with_status;
        let peers_with_status = self.registry.peers_with_status();
        // The commit-pipeline counters now live on the Sequencer task; read them
        // from the latest published view snapshot.
        let view = self.last_view;
        let submitted_applies = view.submitted_applying_count;
        let (received_bytes_per_sec, received_blocks_per_sec) = self
            .state
            .received_throughput
            .lock()
            .map(|meter| (meter.bytes_per_sec(), meter.blocks_per_sec()))
            .unwrap_or((0, 0));
        self.emit_trace(bs_trace::BLOCK_SYNC_STATE, |row| {
            bs_insert_height(row, bs_trace::BODY_DOWNLOAD_FLOOR, view.floor);
            bs_insert_height(row, bs_trace::VERIFIED_BLOCK_TIP, view.verified_tip);
            bs_insert_height(row, bs_trace::BEST_HEADER_TIP, self.state.best_header_tip);
            bs_insert_u64(row, bs_trace::BODY_LAG, u64::from(self.body_lag()));
            bs_insert_u64(row, bs_trace::APPLYING, view.applying_len);
            bs_insert_u64(row, bs_trace::SUBMITTED_APPLIES, submitted_applies);
            bs_insert_u64(row, bs_trace::REORDER, view.reorder_len);
            bs_insert_u64(row, bs_trace::OUTSTANDING, outstanding as u64);
            if let Some(floor_gap) = floor_gap {
                bs_insert_height(row, bs_trace::FLOOR_GAP_HEIGHT, floor_gap.height);
                bs_insert_str(row, bs_trace::FLOOR_GAP_STATE, floor_gap.state);
                bs_insert_u64(
                    row,
                    bs_trace::FLOOR_GAP_SERVABLE_PEERS,
                    floor_gap.servable_peers as u64,
                );
                bs_insert_u64(
                    row,
                    bs_trace::FLOOR_GAP_AVAILABLE_PEERS,
                    floor_gap.available_peers as u64,
                );
                bs_insert_u64(
                    row,
                    bs_trace::FLOOR_GAP_OUTSTANDING_PEERS,
                    floor_gap.outstanding_peers as u64,
                );
                if let Some(age) = floor_gap.oldest_outstanding_ms {
                    bs_insert_u64(row, bs_trace::FLOOR_GAP_OLDEST_OUTSTANDING_MS, age);
                }
                if let Some(deadline) = floor_gap.next_deadline_ms {
                    bs_insert_u64(row, bs_trace::FLOOR_GAP_NEXT_DEADLINE_MS, deadline);
                }
            }
            bs_insert_u64(
                row,
                bs_trace::BUDGET_AVAILABLE,
                self.state.budget.available(),
            );
            bs_insert_u64(row, bs_trace::BUDGET_RESERVED, self.state.budget.reserved());
            bs_insert_u64(row, bs_trace::PEERS, self.state.peers.len() as u64);
            bs_insert_u64(row, bs_trace::PEERS_WITH_STATUS, peers_with_status as u64);
            // Peers that could be issued work but have no free slots are
            // saturated; the remainder want slots. If those exist and the budget
            // can't fund another worst-case block, the download path is
            // budget-limited (not peer- or work-limited) — the key throughput
            // signal toward the 1–2 Gbps target.
            let peers_wanting_slots = peers_with_status.saturating_sub(slot_saturated_peers);
            let download_blocked_on_budget = u64::from(
                peers_wanting_slots > 0
                    && self.state.budget.available() < BS_PER_BLOCK_WORST_CASE_BYTES,
            );
            bs_insert_u64(
                row,
                bs_trace::PEERS_WANTING_SLOTS,
                peers_wanting_slots as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::DOWNLOAD_BLOCKED_ON_BUDGET,
                download_blocked_on_budget,
            );
            bs_insert_u64(
                row,
                bs_trace::RECEIVED_BYTES_PER_SEC,
                received_bytes_per_sec,
            );
            bs_insert_u64(
                row,
                bs_trace::RECEIVED_BLOCKS_PER_SEC,
                received_blocks_per_sec,
            );
            bs_insert_u64(
                row,
                bs_trace::COMMITTED_BYTES_PER_SEC,
                view.committed_bytes_per_sec,
            );
            bs_insert_u64(
                row,
                bs_trace::COMMITTED_BLOCKS_PER_SEC,
                view.committed_blocks_per_sec,
            );
            bs_insert_u64(row, "inbound_peers", inbound_peers as u64);
            bs_insert_u64(row, "outbound_peers", outbound_peers as u64);
            bs_insert_u64(
                row,
                "inbound_peers_with_status",
                inbound_peers_with_status as u64,
            );
            bs_insert_u64(
                row,
                "outbound_peers_with_status",
                outbound_peers_with_status as u64,
            );
            bs_insert_u64(row, "request_slot_capacity", slot_capacity as u64);
            bs_insert_u64(
                row,
                "request_slot_effective_window",
                slot_effective_window as u64,
            );
            bs_insert_u64(row, "request_slot_available", slot_available as u64);
            bs_insert_u64(
                row,
                "request_slot_timeout_recovery",
                slot_timeout_recovery as u64,
            );
            bs_insert_u64(
                row,
                "request_slot_saturated_peers",
                slot_saturated_peers as u64,
            );
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
                self.state.work.pending_run_count() as u64,
            );
            bs_insert_u64(
                row,
                bs_trace::QUEUE_BLOCKS,
                self.state.work.pending_len() as u64,
            );
            if let Some(start) = self.state.work.min_pending() {
                bs_insert_height(row, bs_trace::QUEUE_MIN_START, start);
            }
            bs_insert_u64(
                row,
                bs_trace::ASSIGNED_LEN,
                self.state.work.in_flight_len() as u64,
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
            if let Some(end) = self.state.work.max_in_flight() {
                bs_insert_height(row, bs_trace::COVERED_MAX_END, end);
            }
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
            bs_insert_status_caps(row, status);
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

    fn trace_message_sent(
        &self,
        peer: &ZakuraPeerId,
        msg: &BlockSyncMessage,
        result: &'static str,
        elapsed: Duration,
    ) {
        self.emit_trace(bs_trace::BLOCK_MESSAGE_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_str(row, bs_trace::KIND, block_sync_message_label(msg));
            bs_insert_str(row, bs_trace::RESULT, result);
            bs_insert_duration_ms(row, bs_trace::ELAPSED_MS, elapsed);
            trace_block_sync_message_fields(row, msg);
        });
    }

    fn trace_apply_finished(
        &self,
        height: block::Height,
        token: BlockApplyToken,
        result: BlockApplyResult,
        budget_reserved_after: u64,
    ) {
        self.emit_trace(bs_trace::BLOCK_APPLY_FINISHED, |row| {
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::APPLY_TOKEN, token);
            bs_insert_str(row, bs_trace::RESULT, block_apply_result_label(result));
            bs_insert_u64(row, bs_trace::BUDGET_RESERVED_AFTER, budget_reserved_after);
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
        prepare_elapsed: Option<Duration>,
        send_elapsed: Duration,
        total_elapsed: Option<Duration>,
    ) {
        self.emit_trace(bs_trace::BLOCK_RANGE_RESPONSE_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(sent_count));
            bs_insert_u64(row, bs_trace::EXPECTED_COUNT, u64::from(requested_count));
            bs_insert_u64(row, bs_trace::SERIALIZED_BYTES, sent_bytes);
            bs_insert_str(row, bs_trace::REASON, reason);
            if let Some(prepare_elapsed) = prepare_elapsed {
                bs_insert_duration_ms(row, bs_trace::PREPARE_ELAPSED_MS, prepare_elapsed);
            }
            bs_insert_duration_ms(row, bs_trace::SEND_ELAPSED_MS, send_elapsed);
            if let Some(total_elapsed) = total_elapsed {
                bs_insert_duration_ms(row, bs_trace::ELAPSED_MS, total_elapsed);
            }
        });
    }

    /// Trace a WorkQueue producer extend (heights newly added to `pending`).
    fn trace_work_extended(&self, inserted: usize) {
        if !self.startup.trace.is_enabled() {
            return;
        }
        self.emit_trace(bs_trace::BLOCK_WORK_EXTENDED, |row| {
            bs_insert_u64(row, bs_trace::RANGE_COUNT, inserted as u64);
            bs_insert_u64(
                row,
                bs_trace::QUEUE_BLOCKS,
                self.state.work.pending_len() as u64,
            );
        });
    }

    /// Trace a WorkQueue take (a contiguous chunk claimed by a peer for issuance).
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

    fn floor_gap_diagnostics(&self, _now: Instant) -> Option<FloorGapDiagnostics> {
        let height = next_height(self.committed_floor)?;
        if height > self.state.best_header_tip {
            return None;
        }

        // Servable / outstanding peer counts come from the registry (the routines
        // mirror their outstanding heights there). The per-request deadline ages
        // now live in the routines and are no longer reactor-visible; this trace
        // field drops the `oldest/next deadline ms` breakdown (S4) — the periodic
        // `BLOCK_SYNC_STATE` row still carries the slot/budget signals.
        let (servable_peers, outstanding_peers) = self.registry.floor_gap_servable(height);
        let available_peers = 0usize;
        let oldest_outstanding_ms = None;
        let next_deadline_ms = None;

        // S3b: the Sequencer's per-height `applying`/`submitted_apply`/`reorder`
        // membership is no longer reactor-visible (it lives on the task). A height
        // held in any of those buffers is in `work.in_flight` (the structural
        // invariant), so it classifies here as `outstanding` (a peer holds the
        // request) or `in_flight_without_outstanding` (taken/buffered, no live
        // request). This trace field loses that finer commit-pipeline breakdown;
        // the periodic `BLOCK_SYNC_STATE` row still carries the reorder/applying
        // counts from the view.
        let state = if outstanding_peers > 0 {
            "outstanding"
        } else if self.state.work.pending_contains(height) {
            "queued"
        } else if self.state.work.in_flight_contains(height) {
            // Held in `in_flight` but no peer has an outstanding request for it:
            // a taken-then-buffered/applying height (or one whose holder dropped).
            "in_flight_without_outstanding"
        } else if self.state.needed_heights.binary_search(&height).is_ok() {
            "needed_unscheduled"
        } else {
            "absent"
        };

        Some(FloorGapDiagnostics {
            height,
            state,
            servable_peers,
            available_peers,
            outstanding_peers,
            oldest_outstanding_ms,
            next_deadline_ms,
        })
    }

    fn publish_metrics(&self) {
        // These lossy casts are metrics-only gauges; consensus and scheduling
        // continue to use the original integer values.
        metrics::gauge!("sync.block.best_header_tip.height")
            .set(self.state.best_header_tip.0 as f64);
        metrics::gauge!("sync.block.verified_tip.height").set(self.verified_block_tip.0 as f64);
        metrics::gauge!("sync.block.missing_bodies").set(self.state.needed_heights.len() as f64);
        metrics::gauge!("sync.block.budget.reserved_bytes")
            .set(self.state.budget.reserved() as f64);
        metrics::gauge!("sync.block.reorder.buffered_bytes")
            .set(self.last_view.reorder_buffered_bytes as f64);
        metrics::gauge!("sync.block.applying").set(self.last_view.applying_len as f64);
        // Outstanding (unreceived in-flight) heights summed across peers from the
        // registry (the routines own the per-peer outstanding now).
        metrics::gauge!("sync.block.outstanding").set(self.registry.total_unreceived() as f64);
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
    /// stalled driver wedge the reactor. Required driver actions wait up to
    /// [`ACTION_SEND_TIMEOUT`]; past that the action is dropped so the reactor keeps
    /// draining peer-lifecycle events, request timeouts, and misbehavior
    /// disconnects. Returns `true` only if the action was accepted.
    async fn dispatch_action(&self, action: BlockSyncAction) -> bool {
        self.trace_action_dispatched(&action);
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
        // The aggregate soft-disconnect count lives in the registry so it is shared
        // across the routine (download offenses) and the reactor (serving offenses).
        let should_cancel = self
            .registry
            .record_misbehavior(&peer, block_sync_misbehavior_is_soft(reason));
        if should_cancel {
            if let Some(peer_state) = self.state.peers.get(&peer) {
                peer_state.session.cancel_token().cancel();
            }
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

pub(super) fn bs_insert_height(
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

pub(super) fn bs_insert_u64(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    value: u64,
) {
    row.insert(key.to_string(), serde_json::Value::from(value));
}

fn bs_insert_duration_ms(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    duration: Duration,
) {
    bs_insert_u64(
        row,
        key,
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    );
}

fn bs_insert_status_caps(
    row: &mut serde_json::Map<String, serde_json::Value>,
    status: BlockSyncStatus,
) {
    bs_insert_u64(
        row,
        bs_trace::MAX_BLOCKS_PER_RESPONSE,
        u64::from(status.max_blocks_per_response),
    );
    bs_insert_u64(
        row,
        bs_trace::MAX_INFLIGHT_REQUESTS,
        u64::from(status.max_inflight_requests),
    );
    bs_insert_u64(
        row,
        bs_trace::MAX_RESPONSE_BYTES,
        u64::from(status.max_response_bytes),
    );
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
            bs_insert_status_caps(row, *status);
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

pub(super) fn block_sync_message_label(msg: &BlockSyncMessage) -> &'static str {
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

pub(super) fn tolerated_bytes(reserved_bytes: u64, tolerance_percent: u32) -> u64 {
    reserved_bytes.saturating_mul(u64::from(tolerance_percent.max(100))) / 100
}

pub(super) fn block_sync_misbehavior_is_soft(reason: BlockSyncMisbehavior) -> bool {
    matches!(
        reason,
        BlockSyncMisbehavior::SizeMismatch
            | BlockSyncMisbehavior::RangeUnavailable
            | BlockSyncMisbehavior::GetBlocksSpam
    )
}
