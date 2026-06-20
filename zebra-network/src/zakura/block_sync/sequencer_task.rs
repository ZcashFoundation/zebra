//! The Sequencer's own serial task (S3b boundary split).
//!
//! S3b moves the consensus-critical commit pipeline (`Sequencer`: reorder →
//! applying → `SubmitBlock` → apply-finished) off the reactor's single thread
//! and into this spawned serial task. The reactor keeps issuance, peer matching,
//! serving, and the producer; it forwards every Sequencer-mutating event over a
//! bounded ordered input channel ([`SequencerInput`]) and learns committed
//! progress back over a non-blocking `watch` ([`SequencerView`]).
//!
//! The logic in each input handler is the **verbatim** logic that used to run
//! inline in the matching reactor handler (`handle_block`'s body-acceptance tail,
//! `apply_state_frontiers_changed`'s Sequencer half, `handle_chain_tip_reset`,
//! `handle_block_apply_finished`); only its location and the budget/work/actions
//! handles it uses move here. See the design doc §6 "S3b — RESOLVED DESIGN".

use super::{
    events::*,
    reactor::{bs_insert_height, bs_insert_u64},
    sequencer::*,
    state::*,
    work_queue::WorkQueue,
    *,
};

/// A received body the reactor matched (or accepted unmatched) and forwards to
/// the commit pipeline. This is the A2 backpressure path: a slow verifier blocks
/// the task on `actions.send(SubmitBlock)`, the task stops draining input, the
/// bounded input channel fills, and the reactor blocks on `input_tx.send`.
#[derive(Clone, Debug)]
pub(super) struct SequencedBody {
    pub(super) height: block::Height,
    pub(super) hash: block::Hash,
    pub(super) block: Arc<block::Block>,
    pub(super) bytes: u64,
    pub(super) peer: ZakuraPeerId,
}

/// Every Sequencer-mutating event, forwarded by the reactor in event-demux order
/// so the task processes body/frontier/reset/apply in the same total order the
/// single-task version did (consensus ordering identical).
#[derive(Clone, Debug)]
pub(super) enum SequencerInput {
    /// A matched/queued body to offer to the commit pipeline.
    AcceptBody(SequencedBody),
    /// A verified-tip advance (frontier growth/commit).
    FrontierAdvance {
        frontiers: BlockSyncFrontiers,
        release_applied: bool,
    },
    /// A chain-tip reset (reorg/checkpoint/coalesced update). The two `peer_*`
    /// bools are the peer-outstanding-derived halves of the reset decision,
    /// precomputed by the reactor (which owns peer state); the task ORs them with
    /// its own Sequencer-internal predicates.
    FrontierReset {
        frontiers: BlockSyncFrontiers,
        preserve_active_successors: bool,
        /// `peers.any(outstanding.end_height() >= tip+1)` — half of
        /// `has_active_successor_after`.
        peer_has_successor_after: bool,
        /// `peers.any(outstanding.expected_hash(tip) is Some(h) && h != hash)` —
        /// the peer-outstanding clause of `reset_tip_conflicts_with_local_work`.
        peer_outstanding_conflicts_at_tip: bool,
    },
    /// A verifier apply completion.
    ApplyFinished {
        token: BlockApplyToken,
        height: block::Height,
        hash: block::Hash,
        result: BlockApplyResult,
        local_frontier: Option<BlockSyncFrontiers>,
    },
}

/// The committed view the reactor reacts to. A `watch` (latest-wins) send never
/// blocks, so the task never blocks on the reactor and the bounded input channel
/// cannot deadlock against it.
#[derive(Copy, Clone, Debug)]
pub(super) struct SequencerView {
    pub(super) verified_tip: block::Height,
    pub(super) verified_hash: block::Hash,
    pub(super) floor: block::Height,
    pub(super) finalized: block::Height,
    /// Increments only when the task performs a destructive `reset_to`, so the
    /// reactor distinguishes an advance (drop outstanding *through* tip) from a
    /// reset (drop *all* outstanding).
    pub(super) reset_epoch: u64,
    /// Increments once per processed frontier/reset/apply input (NOT per accepted
    /// body). The reactor runs its heavy serving/producer/schedule reaction only
    /// when this advances, mirroring the single-task version where a pure body
    /// buffer/submit reran nothing but the forwarding peer's reschedule, while a
    /// frontier advance, reset, or apply-finished always reran query/schedule.
    pub(super) reaction_epoch: u64,
    pub(super) reorder_len: u64,
    pub(super) applying_len: u64,
    pub(super) reorder_buffered_bytes: u64,
    pub(super) submitted_applying_count: u64,
    pub(super) committed_bytes_per_sec: u64,
    pub(super) committed_blocks_per_sec: u64,
}

/// Build the initial view from the startup frontiers, before the task runs.
pub(super) fn initial_view(frontiers: BlockSyncFrontiers) -> SequencerView {
    SequencerView {
        verified_tip: frontiers.verified_block_tip,
        verified_hash: frontiers.verified_block_hash,
        floor: frontiers.verified_block_tip,
        finalized: frontiers.finalized_height,
        reset_epoch: 0,
        reaction_epoch: 0,
        reorder_len: 0,
        applying_len: 0,
        reorder_buffered_bytes: 0,
        submitted_applying_count: 0,
        committed_bytes_per_sec: 0,
        committed_blocks_per_sec: 0,
    }
}

/// The serial commit-pipeline task. Owns the `Sequencer` (moved out of state), a
/// `ByteBudget` clone, an `Arc<WorkQueue>` clone, an action sender clone, and the
/// committed throughput meter. Releases bytes directly and emits `SubmitBlock` /
/// `Misbehavior` on the same action channel the reactor uses.
pub(super) struct SequencerTask {
    sequencer: Sequencer,
    budget: ByteBudget,
    work: Arc<WorkQueue>,
    actions: mpsc::Sender<BlockSyncAction>,
    committed_throughput: ThroughputMeter,
    /// Tracks the finalized height so the published view carries it forward; the
    /// reactor folds it into its `finalized_height` mirror with a `max`.
    finalized_height: block::Height,
    verified_block_hash: block::Hash,
    reset_epoch: u64,
    reaction_epoch: u64,
    input_rx: mpsc::Receiver<SequencerInput>,
    view_tx: watch::Sender<SequencerView>,
    action_send_timeout: Duration,
    trace: ZakuraTrace,
}

impl SequencerTask {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        sequencer: Sequencer,
        budget: ByteBudget,
        work: Arc<WorkQueue>,
        actions: mpsc::Sender<BlockSyncAction>,
        committed_throughput: ThroughputMeter,
        frontiers: BlockSyncFrontiers,
        input_rx: mpsc::Receiver<SequencerInput>,
        view_tx: watch::Sender<SequencerView>,
        action_send_timeout: Duration,
        trace: ZakuraTrace,
    ) -> Self {
        Self {
            sequencer,
            budget,
            work,
            actions,
            committed_throughput,
            finalized_height: frontiers.finalized_height,
            verified_block_hash: frontiers.verified_block_hash,
            reset_epoch: 0,
            reaction_epoch: 0,
            input_rx,
            view_tx,
            action_send_timeout,
            trace,
        }
    }

    pub(super) async fn run(mut self) {
        while let Some(input) = self.input_rx.recv().await {
            // Each handler reports whether it did work that the single-task
            // version would have followed with the reactor's heavy
            // serving/producer/schedule tail. Bumping `reaction_epoch` only then
            // keeps the reactor from re-querying/-scheduling on a pure body
            // buffer/submit or a no-op (stale/duplicate) apply completion.
            let needs_reaction = match input {
                SequencerInput::AcceptBody(body) => {
                    self.handle_accept_body(body).await;
                    false
                }
                SequencerInput::FrontierAdvance {
                    frontiers,
                    release_applied,
                } => {
                    self.handle_frontier_advance(frontiers, release_applied)
                        .await;
                    true
                }
                SequencerInput::FrontierReset {
                    frontiers,
                    preserve_active_successors,
                    peer_has_successor_after,
                    peer_outstanding_conflicts_at_tip,
                } => {
                    self.handle_frontier_reset(
                        frontiers,
                        preserve_active_successors,
                        peer_has_successor_after,
                        peer_outstanding_conflicts_at_tip,
                    )
                    .await;
                    true
                }
                SequencerInput::ApplyFinished {
                    token,
                    height,
                    hash,
                    result,
                    local_frontier,
                } => {
                    self.handle_apply_finished(token, height, hash, result, local_frontier)
                        .await
                }
            };
            if needs_reaction {
                self.reaction_epoch = self.reaction_epoch.saturating_add(1);
            }
            self.publish_view();
        }
    }

    /// Body-acceptance tail (verbatim from `handle_block` ~885-907 and
    /// `accept_unmatched_queued_body` ~1170-1183): offer the body, release on
    /// `Redundant`, then drain ready prefix into applying and submit.
    async fn handle_accept_body(&mut self, body: SequencedBody) {
        match self
            .sequencer
            .accept_body(body.height, body.hash, body.block, body.bytes, body.peer)
        {
            AcceptOutcome::Buffered { .. } => {}
            AcceptOutcome::Redundant { release_bytes } => {
                self.budget.release(release_bytes);
            }
        }
        self.release_contiguous_blocks().await;
    }

    /// Sequencer half of `apply_state_frontiers_changed` (verbatim from
    /// reactor.rs ~447-478, including the stale guard).
    async fn handle_frontier_advance(
        &mut self,
        frontiers: BlockSyncFrontiers,
        release_applied: bool,
    ) {
        // Fold the finalized height forward unconditionally (matches the original's
        // first line), then drop a stale update. The verified tip is monotonic: an
        // advance whose target is below our verified tip must be a no-op, never a
        // regression. This guard is the original `apply_state_frontiers_changed`'s
        // `verified_block_tip < verified_tip() => return None`; without it the
        // second growth-reset path (`< floor`, which permits `< verified_tip`) would
        // call `advance_verified_tip` with a lower tip and regress it.
        self.finalized_height = self.finalized_height.max(frontiers.finalized_height);
        if frontiers.verified_block_tip < self.sequencer.verified_tip() {
            return;
        }
        self.verified_block_hash = frontiers.verified_block_hash;
        let advance = self
            .sequencer
            .advance_verified_tip(frontiers.verified_block_tip, release_applied);
        self.budget.release(advance.release_bytes);
        if advance.changed {
            self.work.advance_floor(frontiers.verified_block_tip);
            self.release_contiguous_blocks().await;
        }
    }

    /// The Sequencer/work/budget body of `handle_chain_tip_reset` (verbatim from
    /// reactor.rs 502-576). The peer-outstanding reads are replaced by the
    /// precomputed `peer_*` bools.
    async fn handle_frontier_reset(
        &mut self,
        frontiers: BlockSyncFrontiers,
        preserve_active_successors: bool,
        peer_has_successor_after: bool,
        peer_outstanding_conflicts_at_tip: bool,
    ) {
        let reset_tip_matches_local_work = !self.reset_tip_conflicts_with_local_work(
            &frontiers,
            frontiers.verified_block_tip <= self.sequencer.floor(),
            peer_outstanding_conflicts_at_tip,
        );

        // State can report a forward `Reset` while checkpoint commits advance
        // under already-submitted or still-downloading successor bodies. Treat
        // that as verified growth once it is inside our submitted/downloaded
        // floor, or when we already have successor work in flight. Keep fork
        // resets destructive when they are not anchored by active successor
        // work.
        if frontiers.verified_block_tip > self.sequencer.verified_tip()
            && (frontiers.verified_block_tip <= self.sequencer.floor()
                || self.has_active_successor_after(
                    frontiers.verified_block_tip,
                    peer_has_successor_after,
                ))
            && reset_tip_matches_local_work
        {
            // Growth-classified reset: treat as a frontier advance (same as the
            // reactor's `handle_state_frontiers_changed` path), `release_applied`.
            self.handle_frontier_advance(frontiers, true).await;
            return;
        }

        metrics::counter!("sync.block.reorg.reset").increment(1);

        // A `Reset` can also be a stale or coalesced state update for a tip
        // already inside our contiguous submitted/downloaded body floor. Do not
        // destructively clear successor bodies in that case: a stale reset
        // snapshot can otherwise erase `applying`/covered state and re-request
        // the same bodies while their first apply is still in flight.
        if preserve_active_successors
            && frontiers.verified_block_tip < self.sequencer.floor()
            && reset_tip_matches_local_work
            && self
                .has_active_successor_after(frontiers.verified_block_tip, peer_has_successor_after)
            && self.active_successor_links_to_anchor(
                frontiers.verified_block_tip,
                frontiers.verified_block_hash,
            )
        {
            self.handle_frontier_advance(frontiers, true).await;
            return;
        }

        let remember_released_applies = frontiers.verified_block_tip > frontiers.finalized_height
            && frontiers.verified_block_tip <= self.sequencer.floor();

        self.finalized_height = frontiers.finalized_height;
        self.verified_block_hash = frontiers.verified_block_hash;

        // The Sequencer pins its verified tip and floor to the reset target and
        // clears the reorder/applying buffers, returning the freed bytes for
        // release.
        let released = self
            .sequencer
            .reset_to(frontiers.verified_block_tip, remember_released_applies);
        self.budget.release(released);
        // Drop every download work item above the reset target (their buffers
        // were cleared by `reset_to`); the reactor's `query_needed_blocks`
        // re-fills.
        self.work.reset_above(self.sequencer.floor());
        // A destructive reset: bump the epoch so the reactor drops *all*
        // outstanding requests (not just those through the tip).
        self.reset_epoch = self.reset_epoch.saturating_add(1);
    }

    /// Verbatim from `handle_block_apply_finished` (1443-1540), minus the
    /// reactor-side serving/query/schedule/status tail (which the view reaction
    /// runs). The embedded `local_frontier` advance is folded in as a frontier
    /// advance with `release_applied: false`.
    async fn handle_apply_finished(
        &mut self,
        token: BlockApplyToken,
        height: block::Height,
        hash: block::Hash,
        result: BlockApplyResult,
        local_frontier: Option<BlockSyncFrontiers>,
    ) -> bool {
        // A stale completion (no live applying entry, or token/hash mismatch)
        // only decrements the submitted-apply record and returns; the single-task
        // version ran no query/schedule tail here, so it needs no reaction.
        let Some((applying_token, applying_hash)) = self.sequencer.applying_token_hash(height)
        else {
            self.sequencer.decrement_submitted_apply(height, hash);
            return false;
        };
        if applying_hash != hash || applying_token != token {
            self.sequencer.decrement_submitted_apply(height, hash);
            return false;
        }

        let accepted_local_frontier = if let Some(frontiers) = local_frontier {
            // Fold the `local_frontier` advance in as a frontier advance without
            // releasing committed applying bodies (`release_applied: false`),
            // matching the inline `apply_state_frontiers_changed(.., false)` call.
            // It is accepted only when it is not a stale (older-tip) update.
            if frontiers.verified_block_tip < self.sequencer.verified_tip() {
                None
            } else {
                self.handle_frontier_advance(frontiers, false).await;
                Some(frontiers)
            }
        } else {
            None
        };

        if matches!(result, BlockApplyResult::Duplicate) && self.sequencer.verified_tip() < height {
            // Stale duplicate for a height we have not verified to: the single-task
            // version ran the serving/query tail only when the accepted local
            // frontier advanced serving (an `old_serving_tip` existed).
            return accepted_local_frontier.is_some();
        }
        let applying = self
            .sequencer
            .remove_applying(height)
            .expect("applying entry exists because it was just checked");

        self.budget.release(applying.bytes);
        // A `Committed` result is a body that newly extended the chain; count it
        // toward commit throughput (the apply rate the download path is racing).
        if matches!(result, BlockApplyResult::Committed) {
            self.committed_throughput.record(applying.bytes);
        }
        self.sequencer.decrement_submitted_apply(height, hash);
        match result {
            BlockApplyResult::Committed | BlockApplyResult::Duplicate => {}
            BlockApplyResult::Rejected | BlockApplyResult::TimedOut
                if height > self.sequencer.verified_tip() =>
            {
                // Drop the rejected body and every successor (in applying and
                // reorder), roll the floor back below it, and drop the WorkQueue
                // entries above the rolled-back floor so the heights are
                // re-requestable (the reactor's `query_needed_blocks` re-fills).
                let released = self.sequencer.release_applying_blocks_from(height);
                self.budget.release(released);
                self.sequencer.reset_floor_below(height);
                self.work.reset_above(self.sequencer.floor());
                let dropped = self.sequencer.drop_reorder_from(height);
                self.budget.release(dropped);
                // A `Rejected` result means consensus found the body invalid.
                // Attribute it to the delivering peer so repeat offenders are
                // scored and eventually disconnected. `TimedOut` is a local apply
                // timeout, not a peer fault, so it is not scored.
                if matches!(result, BlockApplyResult::Rejected) {
                    self.send_action(BlockSyncAction::Misbehavior {
                        peer: applying.source_peer.clone(),
                        reason: BlockSyncMisbehavior::InvalidBlock,
                    })
                    .await;
                }
            }
            BlockApplyResult::Rejected | BlockApplyResult::TimedOut => {}
        }
        if let Some(frontiers) = accepted_local_frontier {
            let released = self
                .sequencer
                .release_applied_through(frontiers.verified_block_tip);
            self.budget.release(released);
        }

        self.release_contiguous_blocks().await;
        true
    }

    /// Drain the contiguous reorder prefix into applying and submit (verbatim
    /// from `release_contiguous_blocks` + `submit_pending_blocks`).
    async fn release_contiguous_blocks(&mut self) {
        let _ = self.sequencer.drain_ready_into_applying();
        self.submit_pending_blocks().await;
    }

    async fn submit_pending_blocks(&mut self) {
        for height in self.sequencer.submittable_heights() {
            let Some(item) = self.sequencer.prepare_submit(height) else {
                continue;
            };

            metrics::counter!("sync.block.submit.sent").increment(1);
            if !self
                .send_action(BlockSyncAction::SubmitBlock {
                    token: item.token,
                    block: item.block,
                })
                .await
            {
                self.sequencer.unsubmit(item.height, item.token);
                return;
            }
            self.sequencer
                .record_submitted_apply(item.height, item.hash);
            self.trace_body_submitted(item.height, item.token);
        }
    }

    fn trace_body_submitted(&self, height: block::Height, token: BlockApplyToken) {
        self.trace.emit_with(BLOCK_SYNC_TABLE, |row| {
            row.insert(
                bs_trace::EVENT.to_string(),
                serde_json::Value::String(bs_trace::BLOCK_BODY_SUBMITTED.to_string()),
            );
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::APPLY_TOKEN, token);
        });
    }

    /// `reset_tip_conflicts_with_local_work`'s Sequencer-internal predicates,
    /// with the peer-outstanding clause supplied by the reactor.
    fn reset_tip_conflicts_with_local_work(
        &self,
        frontiers: &BlockSyncFrontiers,
        ignore_non_material_conflicts: bool,
        peer_outstanding_conflicts_at_tip: bool,
    ) -> bool {
        let height = frontiers.verified_block_tip;
        let hash = frontiers.verified_block_hash;

        if self
            .sequencer
            .reorder_hash(height)
            .is_some_and(|buffered_hash| buffered_hash != hash)
        {
            return true;
        }
        if self
            .sequencer
            .applying_hash(height)
            .is_some_and(|applying_hash| applying_hash != hash)
        {
            return true;
        }
        if !ignore_non_material_conflicts
            && self.sequencer.submitted_has_only_other_hashes(height, hash)
        {
            return true;
        }
        if !ignore_non_material_conflicts && peer_outstanding_conflicts_at_tip {
            return true;
        }
        false
    }

    fn has_active_successor_after(
        &self,
        height: block::Height,
        peer_has_successor_after: bool,
    ) -> bool {
        let Some(next) = next_height(height) else {
            return false;
        };

        self.sequencer.has_buffered_at_or_above(next) || peer_has_successor_after
    }

    fn active_successor_links_to_anchor(
        &self,
        height: block::Height,
        anchor_hash: block::Hash,
    ) -> bool {
        let Some(next) = next_height(height) else {
            return true;
        };

        self.sequencer
            .applying_previous_block_hash(next)
            .map(|previous_block_hash| previous_block_hash == anchor_hash)
            .unwrap_or(true)
    }

    async fn send_action(&self, action: BlockSyncAction) -> bool {
        // `SubmitBlock` is the intended verifier-backpressure point: a slow
        // verifier blocks the task here, stopping it from draining `input`. The
        // timeout matches the reactor's `dispatch_action` so a permanently
        // stalled driver does not wedge the pipeline forever.
        match time::timeout(self.action_send_timeout, self.actions.send(action)).await {
            Ok(Ok(())) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                metrics::counter!("sync.block.action.send_timeout").increment(1);
                false
            }
        }
    }

    fn publish_view(&mut self) {
        self.committed_throughput.sample(Instant::now());
        let _ = self.view_tx.send_replace(SequencerView {
            verified_tip: self.sequencer.verified_tip(),
            verified_hash: self.verified_block_hash,
            floor: self.sequencer.floor(),
            finalized: self.finalized_height,
            reset_epoch: self.reset_epoch,
            reaction_epoch: self.reaction_epoch,
            reorder_len: self.sequencer.reorder_len() as u64,
            applying_len: self.sequencer.applying_len() as u64,
            reorder_buffered_bytes: self.sequencer.reorder_buffered_bytes(),
            submitted_applying_count: self.sequencer.submitted_applying_count() as u64,
            committed_bytes_per_sec: self.committed_throughput.bytes_per_sec(),
            committed_blocks_per_sec: self.committed_throughput.blocks_per_sec(),
        });
    }
}
