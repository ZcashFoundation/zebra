//! Per-peer pipe-routine for Zakura block sync (S4 — the pipe IS the routine).
//!
//! S4 inverts the inbound data flow. One task per connected peer owns its
//! `FramedRecv` (the transport read), decodes each stream-6 frame, AND runs the
//! download logic as a direct continuation in the **same task** — there is no
//! reactor inbound demux and no per-peer `PeerInput` channel. Data flows
//! pipe-routine → reactor (over [`RoutineToReactor`]) for shared concerns only:
//! serving (`GetBlocks`), status advertisement, the producer re-query ping, and
//! serving-side misbehavior. The routine owns its `BlockSyncPeerSession` clone,
//! `outstanding`, the adaptive outbound window + timeout-recovery slots,
//! `received_status`/servable caps, and the want-work fill loop.
//!
//! The one throughput-critical effect: the matched-body
//! `sequencer_input.send(AcceptBody).await` runs in this per-peer task, so a
//! slow verifier (Sequencer backpressure) stalls only one routine, not the whole
//! fleet (design doc §7.4/§9). The download decision gates **only** on the byte
//! budget + per-peer slots — never on the committed/verified/finalized floor
//! (§7.8): `take_in_range(servable_low, servable_high, n)` uses `servable_high` as
//! the *only* upper bound.
//!
//! The download logic is **moved, not rewritten** from the pre-S4 reactor: the
//! want-work fill loop ports `fill_peer`, the matched-body tail ports
//! `handle_block`, and the unmatched fallthroughs port `accept_unmatched_queued_body`
//! / the `ignore_*` helpers / `stale_adjusted_outstanding_disposition` verbatim,
//! changing only where the state lives (now routine-local or the
//! [`PeerRegistry`]) and that inbound now arrives as a decoded frame from this
//! task's own `FramedRecv` rather than a `PeerInput` channel.

use std::collections::{BTreeMap, HashSet};

use tokio::sync::{futures::Notified, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::events::RoutineToReactor;
use super::{
    config::BS_PER_BLOCK_WORST_CASE_BYTES,
    peer_registry::{hard_outbound_capacity, PeerRegistry},
    pipe::block_sync_guard,
    reactor::{
        block_sync_message_label, bs_insert_height, bs_insert_peer, bs_insert_u64, tolerated_bytes,
    },
    request::BlockRangeRequest,
    sequencer_task::{SequencedBody, SequencerInput, SequencerView},
    state::{DownloadWindow, OutstandingBlockRange, ThroughputMeter},
    work_queue::{WorkItem, WorkQueue},
    BlockSyncAction, BlockSyncMessage, BlockSyncMisbehavior, BlockSyncPeerSession, BlockSyncStatus,
    ZakuraBlockSyncConfig, ZakuraPeerId, ZakuraTrace,
};
use crate::zakura::{
    trace::{block_sync_trace as bs_trace, BLOCK_SYNC_TABLE},
    Admit, FramedRecv, SinkReject,
};
use std::{sync::Arc, time::Duration, time::Instant};
use tokio::time;
use zebra_chain::{block, serialization::ZcashSerialize};

/// How long a routine avoids re-taking a height it just returned on a failure
/// (RangeUnavailable / timeout / send-failure / disconnect-retry) before it will
/// contest that height again. The window only has to be long enough that, on the
/// single-threaded test runtime, the other routines woken by the same failure
/// `return_items` get a chance to take the contested work first — it is the
/// peer-local retry bias the central `fill_rotation_cursor` used to provide
/// (design doc §6 S4 "the peer-local timeout bias is re-introduced in S4"). It is
/// negligible against real sync timescales, and the height stays `pending` and
/// fully contestable by every other peer throughout (§7.5).
const RETRY_AVOID_BACKOFF: Duration = Duration::from_millis(50);

/// Outcome classification for finishing an outstanding request (ported verbatim
/// from the reactor's `OutstandingRangeDisposition`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Disposition {
    Satisfied,
    RetryOriginal,
    RetryMissing,
}

/// The per-peer pipe-routine. Owns its `FramedRecv` (transport read), the session
/// clone, the download window, the `outstanding` requests, the servable caps /
/// `received_status` it learns from `Status` frames, and holds clones of the
/// shared primitives. One task per connected peer; spawned at the pipe spawn point
/// (`service::add_peer`) so a protocol reject cancels the whole connection.
pub(super) struct PeerRoutine {
    peer: ZakuraPeerId,
    session: BlockSyncPeerSession,
    config: ZakuraBlockSyncConfig,

    // ---- transport inbound (the pipe half) ----
    /// This peer's ordered stream-6 frame reader. Decoded in the routine's own
    /// task; inbound never flows through the reactor (S4 inverted data flow).
    recv: FramedRecv,

    // ---- per-peer download state (moved out of `PeerBlockState`) ----
    window: DownloadWindow,
    /// Whether this peer has sent a `Status` yet (gates want-work; mirrored into
    /// the registry for the reactor's serving/candidate reads).
    received_status: bool,
    /// This peer's advertised servable range, learned from its `Status`. The
    /// want-work upper bound (§7.8); never the floor.
    servable_low: block::Height,
    servable_high: block::Height,
    /// This peer's clamped advertised serving caps, learned from its `Status`.
    /// Authoritative for the routine's own want-work decision (mirrored into the
    /// registry for the reactor's serving-side reads).
    max_blocks_per_response: u32,
    max_response_bytes: u32,
    /// Rate meter for sending our `Status` reply to this peer's inbound `Status`
    /// (the pre-S4 `unsolicited` meter; the reply decision is routine-local now,
    /// the actual send stays reactor-side via `RoutineToReactor::StatusReceived`).
    status_reply_meter: super::state::RateMeter,
    /// Rate meter gating how often this peer's `Status` frames are applied at all
    /// (the pre-S4 `inbound_status` meter), so a status flood cannot spin the
    /// routine. A status that grows the servable range bypasses the meter.
    inbound_status_meter: super::state::RateMeter,
    /// Heights this routine recently returned on a failure, mapped to the instant
    /// after which it may re-take them. While avoided, the routine leaves the
    /// height `pending` (contestable by any other peer) but does not re-grab it
    /// itself — the peer-local retry bias (see [`RETRY_AVOID_BACKOFF`]). Pruned on
    /// expiry each fill pass.
    retry_avoid: BTreeMap<block::Height, Instant>,

    // ---- shared primitives (clones) ----
    /// Generation this routine was spawned with; gates its registry writes (and
    /// its `Drop`) so a superseded routine (e.g. a session replacement before the
    /// old task's async Drop runs) cannot corrupt the live entry.
    generation: u64,
    budget: super::state::ByteBudget,
    work: Arc<WorkQueue>,
    registry: Arc<PeerRegistry>,
    received_throughput: Arc<std::sync::Mutex<ThroughputMeter>>,
    sequencer_input: mpsc::Sender<SequencerInput>,
    actions: mpsc::Sender<BlockSyncAction>,
    /// Shared routine→reactor channel for serving / status-advertise / re-query /
    /// serving-misbehavior. `try_send` (bounded, never-wedging) so a busy reactor
    /// cannot backpressure this decode loop into stalling the transport.
    routine_to_reactor: mpsc::Sender<RoutineToReactor>,
    view: watch::Receiver<SequencerView>,
    /// Last `reset_epoch` this routine reacted to, so a `view.changed()` can tell
    /// a destructive reset (in-place clear of outstanding) from a plain advance.
    last_reset_epoch: u64,

    /// Cancellation: the peer's service session token. Fires on disconnect, park,
    /// or local shutdown; the routine exits and its `Drop` guard returns work.
    cancel: CancellationToken,
    trace: ZakuraTrace,
}

impl PeerRoutine {
    /// Build a pipe-routine for `peer`. The caller (`service::add_peer`) drives
    /// `run()` inside `spawn_supervised_pipe` so a protocol reject cancels the
    /// whole connection. `generation` is the value obtained from
    /// [`PeerRegistry::admit`](super::peer_registry::PeerRegistry::admit).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        peer: ZakuraPeerId,
        session: BlockSyncPeerSession,
        recv: FramedRecv,
        config: ZakuraBlockSyncConfig,
        generation: u64,
        budget: super::state::ByteBudget,
        work: Arc<WorkQueue>,
        registry: Arc<PeerRegistry>,
        received_throughput: Arc<std::sync::Mutex<ThroughputMeter>>,
        sequencer_input: mpsc::Sender<SequencerInput>,
        actions: mpsc::Sender<BlockSyncAction>,
        routine_to_reactor: mpsc::Sender<RoutineToReactor>,
        view: watch::Receiver<SequencerView>,
        cancel: CancellationToken,
        trace: ZakuraTrace,
    ) -> Self {
        let window = DownloadWindow::new(&config);
        let last_reset_epoch = view.borrow().reset_epoch;
        let status_reply_meter = super::state::RateMeter::new(config.status_refresh_interval);
        let inbound_status_meter = super::state::RateMeter::new(
            config.status_refresh_interval.min(Duration::from_secs(1)),
        );
        // Defer the first Status reply: the reactor already sends a connect-status
        // on `PeerConnected`, so the peer's first inbound `Status` should not also
        // trigger an immediate reply (matches the pre-S4 `peer_connected`
        // `unsolicited.mark_taken`).
        let mut status_reply_meter = status_reply_meter;
        status_reply_meter.mark_taken(Instant::now());
        let max_blocks_per_response = config.advertised_max_blocks_per_response();
        let max_response_bytes = config.advertised_max_response_bytes();
        PeerRoutine {
            peer,
            session,
            config,
            recv,
            window,
            received_status: false,
            servable_low: block::Height::MIN,
            servable_high: block::Height::MIN,
            max_blocks_per_response,
            max_response_bytes,
            status_reply_meter,
            inbound_status_meter,
            retry_avoid: BTreeMap::new(),
            generation,
            budget,
            work,
            registry,
            received_throughput,
            sequencer_input,
            actions,
            routine_to_reactor,
            view,
            last_reset_epoch,
            cancel,
            trace,
        }
    }

    /// Run the pipe-routine until stream close, cancellation, or a protocol
    /// reject. A reject returns `Err(SinkReject::protocol(..))` so the supervised
    /// pipe tears the whole connection down, matching the pre-S4 `run_peer`.
    pub(super) async fn run(mut self) -> Result<(), SinkReject> {
        // Local clones so the `Notified` futures below borrow these handles, not
        // `self` — `self.try_fill()` needs `&mut self` while the notifications are
        // pinned. The clones share the same underlying `Arc`, so the wakes still
        // fire for releases/extends done through the routine's own `self.budget` /
        // `self.work`.
        let budget = self.budget.clone();
        let work = self.work.clone();
        // The per-connection oversize guard the pre-S4 pipe applied at ingress.
        let mut guard = block_sync_guard();
        loop {
            // §7.3 missed-wake safety: register both `Notify`s via
            // `Notified::enable()` BEFORE the fill attempt. The budget/work
            // `Notify`s use `notify_waiters` (no stored permit), so a
            // release/extend that lands between the fill-check and the await
            // would be lost if we registered after — the routine would stall.
            let capacity = budget.subscribe_capacity().notified();
            let available = work.subscribe_available().notified();
            tokio::pin!(capacity);
            tokio::pin!(available);
            Notified::enable(capacity.as_mut());
            Notified::enable(available.as_mut());

            self.try_fill().await;

            // Sleep until the earliest outstanding deadline (own-timeout arm).
            let timeout = self.earliest_deadline_sleep();
            tokio::pin!(timeout);

            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok(()),
                frame = self.recv.recv() => {
                    match frame {
                        // Decode the frame and run the download/serving dispatch
                        // in this same task. A protocol reject propagates out so
                        // the supervised pipe cancels the connection (matches the
                        // pre-S4 `run_peer` reject path); the `Drop` guard returns
                        // unreceived work on the way out.
                        Some(frame) => self.handle_frame(&mut guard, frame).await?,
                        // Stream closed (peer gone): exit cleanly. `Drop` returns
                        // unreceived outstanding heights and releases their budget.
                        None => return Ok(()),
                    }
                }
                changed = self.view.changed() => {
                    match changed {
                        Ok(()) => self.on_view_changed(),
                        // The Sequencer task ended (shutdown); the routine follows.
                        Err(_) => return Ok(()),
                    }
                }
                _ = &mut timeout => {
                    self.expire_due_timeouts(Instant::now());
                }
                _ = &mut capacity => {
                    self.trace_wake("budget_capacity");
                }
                _ = &mut available => {
                    self.trace_wake("work_added");
                }
            }
        }
    }

    /// Admit, decode, and dispatch one inbound frame in this task. `Block` /
    /// `BlocksDone` / `RangeUnavailable` (download) are handled locally; `Status`
    /// updates own servable/caps locally and pings the reactor to advertise;
    /// `GetBlocks` (serving) forwards to the reactor; a decode error reports
    /// `MalformedMessage` and rejects the peer.
    async fn handle_frame(
        &mut self,
        guard: &mut crate::zakura::SessionGuard,
        frame: crate::zakura::Frame,
    ) -> Result<(), SinkReject> {
        match guard.admit(&frame) {
            Admit::Pass => {}
            Admit::Throttle => {
                return Err(SinkReject::local(
                    "block-sync guard unexpectedly throttled an inbound frame",
                ));
            }
            Admit::Reject(reason) => {
                return Err(SinkReject::protocol(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    reason,
                )));
            }
        }

        let frame_payload_bytes = frame.payload.len();
        // Measured here, on the per-peer task, so the body size never has to be
        // recomputed by re-serializing the block on another thread (A1).
        let msg = match BlockSyncMessage::decode_frame(frame) {
            Ok(msg) => msg,
            Err(error) => {
                // A malformed frame is `MalformedMessage` misbehavior AND a fatal
                // protocol reject for the whole connection (matches the pre-S4
                // `run_peer` decode-error path). Report via the shared channel,
                // then reject; the report is best-effort and never blocks.
                let protocol_error =
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string());
                tracing::debug!(peer = ?self.peer, ?error, "malformed Zakura block-sync frame");
                let _ = self
                    .routine_to_reactor
                    .try_send(RoutineToReactor::Misbehavior {
                        peer: self.peer.clone(),
                        reason: BlockSyncMisbehavior::MalformedMessage,
                    });
                return Err(SinkReject::protocol(protocol_error));
            }
        };
        let body_wire_bytes = msg.block_body_wire_bytes(frame_payload_bytes);
        self.trace_message_received(&msg);

        match msg {
            BlockSyncMessage::Status(status) => self.handle_status(status),
            BlockSyncMessage::GetBlocks {
                start_height,
                count,
            } => {
                // Serving is reactor-owned (state query + driver). Forward the
                // request; the reactor serves via the session clone it holds.
                let _ = self
                    .routine_to_reactor
                    .try_send(RoutineToReactor::ServeGetBlocks {
                        peer: self.peer.clone(),
                        start_height,
                        count,
                    });
            }
            BlockSyncMessage::Block(block) => {
                self.trace_wake("own_body");
                self.handle_body(block, body_wire_bytes).await;
            }
            BlockSyncMessage::BlocksDone {
                start_height,
                returned: _,
            } => self.handle_blocks_done(start_height).await,
            BlockSyncMessage::RangeUnavailable {
                start_height,
                count: _,
            } => self.handle_range_unavailable(start_height).await,
        }
        Ok(())
    }

    /// Apply this peer's `Status` locally (servable range, caps, `received_status`)
    /// and into the registry, then ping the reactor to advertise our reply and
    /// republish the candidate. Ports the reactor's pre-S4 `handle_status`
    /// validate / rate-meter / upsert; the servable read for want-work is now this
    /// routine's own fields.
    fn handle_status(&mut self, status: BlockSyncStatus) {
        if status.servable_low > status.servable_high {
            let _ = self
                .routine_to_reactor
                .try_send(RoutineToReactor::Misbehavior {
                    peer: self.peer.clone(),
                    reason: BlockSyncMisbehavior::InvalidStatus,
                });
            return;
        }
        let now = Instant::now();
        // A status is applied if the rate meter allows it OR it grows our servable
        // range (so a peer that just extended its range is never throttled out).
        let grows =
            status.servable_high > self.servable_high || status.servable_low < self.servable_low;
        if !self.inbound_status_meter.try_take(now) && !grows {
            return;
        }
        let send_reply = self.status_reply_meter.try_take(now);
        self.received_status = true;
        self.servable_low = status.servable_low;
        self.servable_high = status.servable_high;
        self.max_blocks_per_response =
            super::config::clamp_advertised_blocks(status.max_blocks_per_response);
        self.max_response_bytes =
            super::config::clamp_advertised_response_bytes(status.max_response_bytes);
        self.window.max_inflight_requests =
            super::config::clamp_advertised_inflight(status.max_inflight_requests);
        // Publish the servable range / clamped caps / received_status to the
        // registry so the reactor's serving/candidate reads and `GetBlocks`
        // admission see them; generation-gated.
        self.registry
            .upsert_status(&self.peer, self.generation, status);
        self.trace_status_received(status);
        // Ask the reactor to advertise our Status reply (if due) and republish the
        // candidate. Best-effort; a full channel just defers the candidate refresh
        // to the next reactor tick.
        let _ = self
            .routine_to_reactor
            .try_send(RoutineToReactor::StatusReceived {
                peer: self.peer.clone(),
                send_reply,
            });
    }

    /// React to a committed-view change: refresh the floor/tip the routine reads,
    /// and on a destructive `reset_epoch` bump clear this routine's outstanding
    /// **in place** (return unreceived heights to `work.pending`, release their
    /// budget, clear the registry outstanding, drop retry-avoid) and re-fan from
    /// the post-`reset_above` `WorkQueue`. The transport is never torn down (§6 S4
    /// "reset = in-place clear, not respawn").
    fn on_view_changed(&mut self) {
        let reset_epoch = self.view.borrow().reset_epoch;
        if reset_epoch == self.last_reset_epoch {
            // A non-destructive advance: the floor/tip the routine reads come
            // straight from the live `view` each time they are needed, so nothing
            // to do but let the want-work loop re-run at the top (a committed
            // floor advance may GC our fully-committed outstanding).
            return;
        }
        self.last_reset_epoch = reset_epoch;
        self.trace_wake("view_reset");
        // The Sequencer already pinned its floor/tip and `work.reset_above`'d the
        // dropped successor heights. Return our unreceived outstanding to
        // `work.pending` (a no-op for heights already dropped from `in_flight` by
        // `reset_above`) and release their reservations exactly once (§7.5).
        for outstanding in self.window.outstanding.drain(..).collect::<Vec<_>>() {
            self.budget.release(outstanding.reserved_bytes());
            self.work.return_items(unreceived_heights(&outstanding));
        }
        self.retry_avoid.clear();
        // Clear our (now-empty) registry outstanding and refresh slot diagnostics.
        self.publish_outstanding();
        // The want-work loop re-fans from the queue at the top of the next
        // iteration (the `reset_above` + producer re-query repopulate `pending`).
    }

    /// Sleep future resolving at the earliest wake the routine schedules for
    /// itself: the soonest outstanding request deadline (own-timeout) **or** the
    /// soonest retry-avoid expiry (so a routine that quiet-returned its only work
    /// re-runs want-work once the bias lifts, even if no external event arrives).
    /// Defaults to a long idle sleep when neither exists.
    fn earliest_deadline_sleep(&self) -> time::Sleep {
        let now = Instant::now();
        let earliest_deadline = self
            .window
            .outstanding
            .iter()
            .map(|outstanding| outstanding.deadline)
            .min();
        let earliest_avoid = self.retry_avoid.values().min().copied();
        let earliest = match (earliest_deadline, earliest_avoid) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        };
        match earliest {
            // Floor the wait at the deadline so a far-future request still wakes
            // promptly; an already-due deadline wakes immediately.
            Some(deadline) => time::sleep(deadline.saturating_duration_since(now)),
            None => time::sleep(Duration::from_secs(3600)),
        }
    }

    // ===================== want-work fill loop (ports `fill_peer`) ===========

    /// Fill this peer's available slots in a single pass, letting the byte budget
    /// (re-checked each iteration via `try_reserve`) be the congestion window.
    /// Ported from the reactor's `fill_peer`; the only changes are where the
    /// per-peer state lives (now routine-local / the registry) and the handles.
    ///
    /// There is no floor gate: downloads are governed solely by the byte budget
    /// and per-peer slots — never floor-distance / near-tip lag (§7.8).
    async fn try_fill(&mut self) {
        let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
        // Reconcile the adaptive window's hard cap with the peer's currently
        // advertised `max_inflight_requests` (it may have grown/shrunk via a
        // `Status`; `handle_status` set `window.max_inflight_requests`). Mirrors
        // the pre-S4 `handle_status` clamp of the window / recovery slots to the
        // new hard capacity.
        let hard = self.window.hard_outbound_capacity();
        self.window.outbound_request_window = self.window.outbound_request_window.min(hard).max(1);
        self.window.timeout_recovery_slots = self.window.timeout_recovery_slots.min(hard);
        // GC this routine's own fully-committed outstanding requests: when the
        // committed floor passes the end of a request, its bodies are no longer
        // needed, so release its reservation and free its slot promptly rather
        // than waiting for the request's own timeout. This is the floor used for
        // GC of *our own* committed requests (§7.8-permitted), never a fetch
        // throttle — it replaces the pre-S4 reactor `drop_outstanding_through`
        // without the cross-peer churn the spec warned about (a partially-received
        // request whose suffix is still above the floor is left in place).
        self.gc_committed_outstanding();
        // Drop expired retry-avoid entries: those heights are contestable by this
        // routine again.
        let now = Instant::now();
        self.retry_avoid.retain(|_, until| *until > now);
        loop {
            if !self.received_status || self.window.available_slots() == 0 {
                break;
            }
            // One contiguous chunk up to the peer's per-request count cap; the
            // outer loop fills the rest of the peer's slots.
            let max_count = usize::try_from(
                self.max_blocks_per_response
                    .min(self.config.advertised_max_blocks_per_response())
                    .max(1),
            )
            .unwrap_or(usize::MAX);
            let (servable_low, servable_high) = (self.servable_low, self.servable_high);

            // Compute this chunk's byte ceiling BEFORE taking any work, from the
            // global byte budget and this peer's per-response cap. The reservation
            // is worst-case per block (it only ever shrinks toward the actual size
            // on receipt, so a valid body is never dropped for a full budget). If
            // the ceiling cannot fund even one worst-case block, break and wait on
            // the budget-capacity / work-added notifications instead of taking a
            // chunk only to return it. Taking-then-returning would call
            // `work.return_items` → `notify_waiters`, which re-wakes THIS routine's
            // own enabled `work_added` notification (registered before the fill) and
            // busy-loops the want-work arm (§7.3 missed-wake's mirror image — a
            // self-wake spin). Gating before the take keeps the routine parked on
            // `capacity` until budget frees up.
            let max_bytes = self
                .budget
                .available()
                .min(u64::from(self.max_response_bytes.max(1)));
            // Cap the chunk taken to what the byte ceiling can fund at worst case;
            // break (without taking) when not even one block fits, so no take/return
            // self-wake cycle can occur.
            let byte_capped_count = if worst == 0 {
                max_count
            } else {
                usize::try_from(max_bytes / worst)
                    .unwrap_or(usize::MAX)
                    .min(max_count)
            };
            if byte_capped_count == 0 {
                break;
            }

            // Take work in this peer's servable range. `servable_high` is NOT
            // clamped to the floor: a peer fetches as far ahead of the committed
            // floor as its servable range and the byte budget allow (§7.8).
            let mut items = self
                .work
                .take_in_range(servable_low, servable_high, byte_capped_count);
            if items.is_empty() {
                break;
            }
            // Peer-local retry bias: if the contiguous chunk we just took leads
            // with heights this routine recently *failed* (RangeUnavailable /
            // timeout / send-failure), quietly put those back so another peer can
            // contest them first, and only keep the suffix this routine is allowed
            // to re-take. `return_items_quiet` does NOT notify (the other peers were
            // already woken by the original failure return), so this cannot
            // self-wake into a take/return spin. If the whole chunk is still
            // avoided, break — the routine wakes to retry when the avoid window
            // expires (see `earliest_deadline_sleep`).
            if !self.retry_avoid.is_empty() {
                let keep_from = items
                    .iter()
                    .position(|(height, _)| !self.retry_avoid.contains_key(height));
                match keep_from {
                    Some(0) => {}
                    Some(index) => {
                        let avoided: Vec<_> = items.drain(..index).map(|(h, _)| h).collect();
                        self.work.return_items_quiet(avoided);
                    }
                    None => {
                        let avoided: Vec<_> = items.iter().map(|(h, _)| *h).collect();
                        self.work.return_items_quiet(avoided);
                        break;
                    }
                }
            }
            self.trace_work_taken(servable_low, servable_high, items.len());

            // `take_in_range` already honoured the byte-capped count, so every
            // taken item fits under `max_bytes`; nothing is returned here.
            let kept_count = items.len();

            let reserved_bytes = worst.saturating_mul(kept_count as u64);
            if !self.budget.try_reserve(reserved_bytes) {
                self.return_taken_items(&items);
                break;
            }

            let count = match u32::try_from(kept_count) {
                Ok(count) => count,
                Err(_) => {
                    self.budget.release(reserved_bytes);
                    self.return_taken_items(&items);
                    break;
                }
            };
            let request = BlockRangeRequest {
                start_height: items[0].0,
                count,
                anchor_hash: items[0].1.hash,
                // The reserved worst-case total (released on a send failure
                // below); distinct from the size estimates in `expected_bytes`.
                estimated_bytes: reserved_bytes,
                expected_hashes: items
                    .iter()
                    .map(|(height, item)| (*height, item.hash))
                    .collect(),
                expected_bytes: items
                    .iter()
                    .map(|(height, item)| (*height, item.estimated_bytes))
                    .collect(),
            };

            let queued_at = Instant::now();
            if let Err(error) = self
                .session
                .try_send_get_blocks(request.start_height, request.count)
            {
                tracing::debug!(
                    peer = ?self.peer,
                    start_height = ?request.start_height,
                    count = request.count,
                    ?error,
                    "failed to queue Zakura block-sync GetBlocks"
                );
                self.session.cancel_token().cancel();
                self.budget.release(request.estimated_bytes);
                // Nothing was received, so return every taken height to the queue.
                self.return_taken_items(&items);
                break;
            }

            let deadline = queued_at + self.config.request_timeout;
            metrics::counter!("sync.block.request.sent").increment(1);
            self.window.record_outbound_request_scheduled();
            self.window.outstanding.push(OutstandingBlockRange {
                request: request.clone(),
                queued_at,
                deadline,
                received: HashSet::new(),
            });
            self.publish_outstanding();
            self.trace_get_blocks_sent(
                request.start_height,
                request.count,
                request.estimated_bytes,
            );
        }

        // If pending work is running low, ping the reactor to re-query (the
        // producer self-gates on low-water, so this is idempotent/cheap).
        if self.work.pending_len() < self.refill_low_water_blocks() {
            let _ = self
                .routine_to_reactor
                .try_send(RoutineToReactor::RequeryNeeded);
        }
    }

    /// Refill low-water mark in blocks (ported from the reactor's
    /// `refill_low_water_blocks`, but for a single peer's caps).
    fn refill_low_water_blocks(&self) -> usize {
        let max_blocks_per_response =
            usize::try_from(self.config.advertised_max_blocks_per_response()).unwrap_or(usize::MAX);
        let max_inflight_per_peer = hard_outbound_capacity(self.window.max_inflight_requests);
        max_inflight_per_peer
            .saturating_mul(max_blocks_per_response)
            .max(max_blocks_per_response)
    }

    /// Put back a chunk this routine took but is not issuing this fill pass
    /// (budget race / send failure). Quiet (no notify): the returning routine must
    /// not re-wake its own want-work arm into a take/return spin, and any other
    /// peer waiting on budget capacity is woken by the matching `budget.release`.
    fn return_taken_items(&self, items: &[(block::Height, WorkItem)]) {
        self.work
            .return_items_quiet(items.iter().map(|(height, _)| *height));
    }

    /// Record heights this routine just returned on a failure so it will not
    /// immediately re-grab them (the peer-local retry bias). The heights stay
    /// `pending` and contestable by every other peer; only this routine defers.
    fn note_retry_avoid(&mut self, heights: impl IntoIterator<Item = block::Height>) {
        let until = Instant::now() + RETRY_AVOID_BACKOFF;
        for height in heights {
            self.retry_avoid.insert(height, until);
        }
    }

    // ===================== own-timeout arm (ports `expire_due_timeouts`) =====

    fn expire_due_timeouts(&mut self, now: Instant) {
        let mut timed_out = Vec::new();
        let mut index = 0;
        while index < self.window.outstanding.len() {
            if self.window.outstanding[index].deadline <= now {
                self.window.reduce_outbound_window_after_timeout();
                timed_out.push(self.window.outstanding.remove(index));
            } else {
                index += 1;
            }
        }
        if timed_out.is_empty() {
            return;
        }
        for outstanding in &timed_out {
            self.budget.release(outstanding.reserved_bytes());
            // Return only the unreceived heights — received ones are buffered (in
            // `in_flight` until committed); re-queuing them would re-fetch a body
            // we already hold (the WorkQueue single-owner invariant forbids it).
            self.return_unreceived_to_queue(outstanding);
        }
        // Bias away from immediately re-grabbing the heights this peer just timed
        // out, so another peer can contest them (the peer-local timeout bias).
        let timed_out_heights: Vec<_> = timed_out.iter().flat_map(unreceived_heights).collect();
        self.note_retry_avoid(timed_out_heights);
        self.publish_outstanding();
    }

    /// Drop this routine's outstanding requests whose whole range is at or below
    /// the committed floor: their bodies are committed and no longer needed, so
    /// release the worst-case reservation still held for any unreceived heights
    /// and free the slot. No heights return to the queue (they are committed,
    /// below the floor, GC'd from the WorkQueue). A partially-committed request
    /// (suffix still above the floor) is left so its remaining bodies keep their
    /// reservation and arrive on the same request.
    fn gc_committed_outstanding(&mut self) {
        let floor = self.committed_floor();
        let mut released = 0u64;
        let mut removed = false;
        let mut index = 0;
        while index < self.window.outstanding.len() {
            if self.window.outstanding[index].request.end_height() <= floor {
                let outstanding = self.window.outstanding.remove(index);
                released = released.saturating_add(outstanding.reserved_bytes());
                removed = true;
            } else {
                index += 1;
            }
        }
        if released > 0 {
            self.budget.release(released);
        }
        if removed {
            self.publish_outstanding();
        }
    }

    // ===================== inbound matched body (ports `handle_block`) ======

    async fn handle_body(&mut self, block: Arc<block::Block>, body_wire_bytes: Option<u64>) {
        let hash = block.hash();
        let Some(height) = block.coinbase_height() else {
            self.report_misbehavior(BlockSyncMisbehavior::InvalidBlock)
                .await;
            return;
        };

        let Some(index) = self.window.outstanding_index_for_height(height) else {
            // No outstanding match — run the unmatched fallthroughs locally.
            if self.ignore_stale_response(height, "body").await {
                return;
            }
            if self
                .accept_unmatched_queued_body(height, hash, block.clone(), body_wire_bytes)
                .await
            {
                return;
            }
            if self.ignore_unmatched_needed_response(height, "body") {
                return;
            }
            if self.ignore_unmatched_active_body_response(height, hash) {
                return;
            }
            if self.ignore_servable_range_response(height, "body") {
                return;
            }
            self.report_misbehavior(BlockSyncMisbehavior::UnsolicitedBlock)
                .await;
            return;
        };
        let outstanding = &self.window.outstanding[index];
        if outstanding.has_received(height) {
            tracing::debug!(peer = ?self.peer, ?height, "ignoring duplicate block-sync body frame");
            return;
        }
        if outstanding.request.expected_hash(height) != Some(hash) {
            self.report_misbehavior(BlockSyncMisbehavior::InvalidBlock)
                .await;
            return;
        }
        let estimated_bytes = outstanding.estimated_bytes_for_height(height).unwrap_or(0);
        let request_start_height = outstanding.request.start_height;
        let request_range_count = outstanding.request.count;
        let request_elapsed_ms = elapsed_ms_u64(outstanding.queued_at.elapsed());

        // The body's transactions are not validated against the header here;
        // consensus does it on apply (`handle_block_apply_finished` attributes a
        // rejection back to the delivering peer for misbehavior scoring).

        // Prefer the wire-measured body size; only re-serialize when absent (test
        // event).
        let serialized_bytes = match body_wire_bytes {
            Some(bytes) => bytes,
            None => match block.zcash_serialize_to_vec() {
                Ok(bytes) => bytes.len() as u64,
                Err(error) => {
                    tracing::debug!(?error, "failed to serialize decoded block-sync body");
                    self.finish_outstanding_at(index, Disposition::RetryOriginal);
                    self.report_misbehavior(BlockSyncMisbehavior::InvalidBlock)
                        .await;
                    return;
                }
            },
        };
        if serialized_bytes > tolerated_bytes(estimated_bytes, self.config.size_deviation_tolerance)
        {
            self.report_misbehavior(BlockSyncMisbehavior::SizeMismatch)
                .await;
        }

        metrics::counter!("sync.block.body.received").increment(1);
        self.record_received(serialized_bytes);
        // The block reserved `BS_PER_BLOCK_WORST_CASE_BYTES` at send time; shrink
        // to the actual size (the reservation only ever decreases, so the budget
        // can never reject a valid body). `mark_received` then stops
        // `reserved_bytes()` counting this height; the only bytes still held are
        // the `serialized_bytes` carried into the reorder buffer.
        self.budget
            .shrink(BS_PER_BLOCK_WORST_CASE_BYTES, serialized_bytes);
        self.trace_body_received(
            height,
            serialized_bytes,
            Some(request_start_height),
            Some(request_range_count),
            Some(request_elapsed_ms),
        );

        let mut completed = None;
        if let Some(outstanding) = self.window.outstanding.get_mut(index) {
            outstanding.mark_received(height);
            if outstanding.is_complete() {
                completed = Some(self.window.outstanding.remove(index));
                self.window.increase_outbound_window_after_success();
            }
        }
        if let Some(outstanding) = completed {
            self.finish_detached(outstanding, Disposition::Satisfied);
        } else {
            self.publish_outstanding();
        }

        // Forward the body to the commit-pipeline task. THE ONLY blocking send in
        // the routine: a slow verifier blocks the task draining input, the bounded
        // input channel fills, and this routine blocks here — backpressure
        // isolated to this peer (the S4 throughput win).
        let _ = self
            .sequencer_input
            .send(SequencerInput::AcceptBody(SequencedBody {
                height,
                hash,
                block,
                bytes: serialized_bytes,
                peer: self.peer.clone(),
            }))
            .await;
        // This body opened only this peer's slots; the want-work loop runs at the
        // top of the next iteration.
    }

    // ===================== unmatched fallthroughs (ported) ==================

    /// Whether a response for `height` is stale (already committed or held). The
    /// held-height portion is recovered through the WorkQueue's `in_flight`
    /// (every buffered/applying height stays claimed until the floor commits past
    /// it). Reads `committed_floor` from the view.
    fn is_stale_response_height(&self, height: block::Height) -> bool {
        height <= self.committed_floor() || self.work.in_flight_contains(height)
    }

    async fn ignore_stale_response(&mut self, height: block::Height, response_kind: &str) -> bool {
        if !self.is_stale_response_height(height) {
            return false;
        }
        tracing::debug!(peer = ?self.peer, ?height, response_kind, "ignoring stale block-sync response");
        true
    }

    /// Ported from the reactor's `accept_unmatched_queued_body`, minus the
    /// disconnected-peer branch (a routine only runs for a live peer): a queued
    /// height served by a peer that lost its original requester returns to the
    /// queue. The routine reserves the body's actual size, claims the height into
    /// `in_flight`, and forwards it.
    async fn accept_unmatched_queued_body(
        &mut self,
        height: block::Height,
        hash: block::Hash,
        block: Arc<block::Block>,
        body_wire_bytes: Option<u64>,
    ) -> bool {
        if self.work.hash_for_height(height) != Some(hash) {
            return false;
        }
        if !self.received_status || height < self.servable_low || height > self.servable_high {
            return false;
        }

        let serialized_bytes = match body_wire_bytes {
            Some(bytes) => bytes,
            None => match block.zcash_serialize_to_vec() {
                Ok(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                Err(error) => {
                    tracing::debug!(
                        peer = ?self.peer,
                        ?height,
                        ?error,
                        "failed to serialize unmatched queued block-sync body"
                    );
                    self.report_misbehavior(BlockSyncMisbehavior::InvalidBlock)
                        .await;
                    return true;
                }
            },
        };

        metrics::counter!("sync.block.response.unmatched_queued_accepted").increment(1);
        self.record_received(serialized_bytes);
        self.trace_body_received(height, serialized_bytes, None, None, None);

        // This queued height owns no prior reservation: reserve its actual size
        // before buffering. If the budget is genuinely full of other legitimate
        // bodies, skip buffering (the height stays queued for retry with its own
        // worst-case reservation, so no valid body is lost overall).
        if !self.budget.try_reserve(serialized_bytes) {
            tracing::debug!(
                peer = ?self.peer,
                ?height,
                serialized_bytes,
                "not buffering unmatched queued block-sync body; height stays queued for retry"
            );
            return true;
        }

        // Claim this height into `in_flight` so it leaves `pending`; if it is
        // already `in_flight` the take is a no-op and the Sequencer drops the
        // later duplicate.
        let _ = self.work.take_in_range(height, height, 1);

        let _ = self
            .sequencer_input
            .send(SequencerInput::AcceptBody(SequencedBody {
                height,
                hash,
                block,
                bytes: serialized_bytes,
                peer: self.peer.clone(),
            }))
            .await;
        true
    }

    fn ignore_unmatched_needed_response(&self, height: block::Height, response_kind: &str) -> bool {
        // The reactor-local `needed_heights` is gone from the routine; the
        // structural equivalent is "the height is still wanted" = pending or
        // in-flight in the WorkQueue (design doc §6 S4 inbound path).
        if !(self.work.pending_contains(height) || self.work.in_flight_contains(height)) {
            return false;
        }
        metrics::counter!("sync.block.response.unmatched_needed_ignored").increment(1);
        tracing::debug!(
            peer = ?self.peer,
            ?height,
            response_kind,
            "ignoring unmatched block-sync response for currently needed height"
        );
        true
    }

    fn ignore_unmatched_active_body_response(
        &self,
        height: block::Height,
        hash: block::Hash,
    ) -> bool {
        if !self.registry.has_outstanding_request(height, hash) {
            return false;
        }
        metrics::counter!("sync.block.response.unmatched_active_ignored").increment(1);
        tracing::debug!(
            peer = ?self.peer,
            ?height,
            "ignoring unmatched block-sync body for height active on another request"
        );
        true
    }

    fn ignore_unmatched_active_terminator_response(&self, start_height: block::Height) -> bool {
        // We reach this only when *this* peer has no outstanding request starting
        // at `start_height`; the registry answers whether another peer is actively
        // requesting a range covering it (cross-peer fanout/retry race), in which
        // case the terminator is dropped quietly rather than scored. A faithful
        // (slightly wider) superset of the pre-S4 start-keyed check.
        if !self.registry.has_outstanding_height(start_height) {
            return false;
        }
        metrics::counter!("sync.block.response.unmatched_active_done_ignored").increment(1);
        tracing::debug!(
            peer = ?self.peer,
            ?start_height,
            "ignoring unmatched block-sync terminator for range active on another request"
        );
        true
    }

    /// An unmatched response for a height the peer *claims to serve*
    /// (`committed_floor < height <= servable_high`) that no other fallthrough
    /// claimed. The common cause is an honest, in-flight body/terminator for a
    /// height we requested before a destructive reset (reorg) then dropped from
    /// our `outstanding` and from `work` (`reset_above`), or one that simply
    /// raced ahead of the producer's asynchronous `work.extend`. The peer asked
    /// for and served this range honestly, so scoring it the *hard*
    /// `UnsolicitedBlock`/`UnsolicitedDone` (immediate, thresholdless disconnect)
    /// would churn honest peers on every reorg. The pre-S3b serial reactor
    /// avoided this by dropping outstanding in the same loop turn as the reset;
    /// the Sequencer-task split (S3b) made that drop asynchronous, opening this
    /// window — restore the no-churn property by dropping the response quietly. A
    /// response *outside* the peer's advertised range is still scored.
    fn ignore_servable_range_response(&self, height: block::Height, response_kind: &str) -> bool {
        if !self.received_status || height <= self.committed_floor() || height > self.servable_high
        {
            return false;
        }
        metrics::counter!("sync.block.response.unmatched_servable_ignored").increment(1);
        tracing::debug!(
            peer = ?self.peer,
            ?height,
            response_kind,
            "ignoring unmatched block-sync response within the peer's servable range"
        );
        true
    }

    // ===================== terminators (ports `handle_blocks_done` etc.) =====

    async fn handle_blocks_done(&mut self, start_height: block::Height) {
        let Some(index) = self.window.outstanding_index_for_start(start_height) else {
            if self.ignore_stale_response(start_height, "terminator").await {
                return;
            }
            if self.ignore_unmatched_needed_response(start_height, "terminator") {
                return;
            }
            if self.ignore_unmatched_active_terminator_response(start_height) {
                return;
            }
            if self.ignore_servable_range_response(start_height, "terminator") {
                return;
            }
            // A known, active peer sent a terminator correlating to no outstanding
            // range, outside the range it claims to serve. Fail closed:
            // `UnsolicitedDone` (a hard misbehavior).
            self.report_misbehavior(BlockSyncMisbehavior::UnsolicitedDone)
                .await;
            return;
        };
        let disposition = self.stale_adjusted_disposition(index, Disposition::RetryMissing);
        self.finish_outstanding_at(index, disposition);
    }

    async fn handle_range_unavailable(&mut self, start_height: block::Height) {
        let Some(index) = self.window.outstanding_index_for_start(start_height) else {
            if self
                .ignore_stale_response(start_height, "unavailable range")
                .await
            {
                return;
            }
            self.trace_range_unavailable(start_height, None, None);
            return;
        };
        let outstanding = &self.window.outstanding[index];
        self.trace_range_unavailable(
            start_height,
            Some(outstanding.request.count),
            Some(elapsed_ms_u64(outstanding.queued_at.elapsed())),
        );
        let disposition = self.stale_adjusted_disposition(index, Disposition::RetryOriginal);
        self.finish_outstanding_at(index, disposition);
    }

    /// Ported from the reactor's `stale_adjusted_outstanding_disposition`: a late
    /// response can still match after the floor moved through its prefix; mark
    /// the stale prefix satisfied and retry only the remaining suffix.
    fn stale_adjusted_disposition(&mut self, index: usize, current: Disposition) -> Disposition {
        let tip = self.committed_floor();
        let Some(outstanding) = self.window.outstanding.get_mut(index) else {
            return current;
        };
        if outstanding.request.start_height > tip {
            return current;
        }
        let released_bytes = outstanding.mark_received_through(tip);
        self.budget.release(released_bytes);
        if outstanding.is_complete() {
            Disposition::Satisfied
        } else {
            Disposition::RetryMissing
        }
    }

    // ===================== outstanding lifecycle (ported) ===================

    fn finish_outstanding_at(&mut self, index: usize, disposition: Disposition) {
        if index >= self.window.outstanding.len() {
            return;
        }
        let outstanding = self.window.outstanding.remove(index);
        self.finish_detached(outstanding, disposition);
    }

    fn finish_detached(&mut self, outstanding: OutstandingBlockRange, disposition: Disposition) {
        self.budget.release(outstanding.reserved_bytes());
        match disposition {
            Disposition::Satisfied => {
                // Every requested height was received and buffered; nothing
                // returns to the queue (buffered heights stay in `in_flight`
                // until the floor commits past them).
            }
            // With fanout = 1 a received height is already buffered and must never
            // be re-fetched, so both retry dispositions return only the unreceived
            // heights to `pending`. `return_items` is idempotent.
            Disposition::RetryOriginal | Disposition::RetryMissing => {
                self.return_unreceived_to_queue(&outstanding);
                // This peer just failed these heights (RangeUnavailable / short
                // BlocksDone): bias away from re-grabbing them so another peer
                // contests the range first (and so the routine cannot self-wake
                // into a re-take spin off its own `return_items`).
                self.note_retry_avoid(unreceived_heights(&outstanding));
            }
        }
        self.publish_outstanding();
    }

    fn return_unreceived_to_queue(&self, outstanding: &OutstandingBlockRange) {
        self.work.return_items(unreceived_heights(outstanding));
    }

    /// Publish this peer's current *unreceived* in-flight height→hash set to the
    /// registry, so the producer's `!has_outstanding_request` filter and the
    /// low-water `total_unreceived` gate read the same per-request-granularity
    /// count the pre-S4 reactor used (`expected_hashes.len() − received.len()`).
    /// Received-but-uncommitted heights are excluded here because they are held in
    /// `work.in_flight` instead — the producer's `!in_flight_contains` clause
    /// already keeps them out of `pending` (design doc §6 S3b/S4).
    fn publish_outstanding(&self) {
        let mut map: BTreeMap<block::Height, block::Hash> = BTreeMap::new();
        for outstanding in &self.window.outstanding {
            for (height, hash) in &outstanding.request.expected_hashes {
                if !outstanding.has_received(*height) {
                    map.insert(*height, *hash);
                }
            }
        }
        if map.is_empty() {
            self.registry.clear_outstanding(&self.peer, self.generation);
        } else {
            self.registry
                .set_outstanding(&self.peer, self.generation, map);
        }
        // Publish the window slot diagnostics for the reactor's periodic trace row
        // (trace only; the reactor lost direct visibility into the routine window).
        let hard_capacity = hard_outbound_capacity(self.window.max_inflight_requests);
        self.registry.publish_slots(
            &self.peer,
            self.generation,
            super::peer_registry::SlotDiagnostics {
                hard_capacity,
                effective_window: hard_capacity.min(self.window.outbound_request_window),
                available_slots: self.window.available_slots(),
                timeout_recovery_slots: self.window.timeout_recovery_slots,
                outstanding_requests: self.window.outstanding.len(),
            },
        );
    }

    // ===================== misbehavior (shared count via registry) ==========

    async fn report_misbehavior(&self, reason: BlockSyncMisbehavior) {
        // Misbehavior is record-only: observe and forward it, but never cancel the
        // session. Peer scoring no longer drives disconnects.
        metrics::counter!("sync.block.peer.violation").increment(1);
        // `Misbehavior` is best-effort: never block the routine.
        let _ = self.actions.try_send(BlockSyncAction::Misbehavior {
            peer: self.peer.clone(),
            reason,
        });
    }

    // ===================== view reads ======================================

    fn committed_floor(&self) -> block::Height {
        self.view.borrow().floor
    }

    fn record_received(&self, bytes: u64) {
        if let Ok(mut meter) = self.received_throughput.lock() {
            meter.record(bytes);
        }
    }

    // ===================== tracing =========================================

    fn emit(
        &self,
        event: &'static str,
        build: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        if !self.trace.is_enabled() {
            return;
        }
        self.trace.emit_with(BLOCK_SYNC_TABLE, |row| {
            row.insert(
                bs_trace::EVENT.to_string(),
                serde_json::Value::String(event.to_string()),
            );
            build(row);
        });
    }

    fn trace_wake(&self, reason: &'static str) {
        self.emit("block_peer_wake", |row| {
            bs_insert_u64(row, "outstanding", self.window.outstanding.len() as u64);
            row.insert(
                "reason".to_string(),
                serde_json::Value::String(reason.to_string()),
            );
        });
    }

    /// Trace a decoded inbound message (the pre-S4 reactor's `trace_message_received`,
    /// now emitted in the routine that decoded it). Records the message kind only;
    /// the per-variant field detail lives on the reactor's heavier trace path.
    fn trace_message_received(&self, msg: &BlockSyncMessage) {
        self.emit(bs_trace::BLOCK_MESSAGE_RECEIVED, |row| {
            row.insert(
                bs_trace::KIND.to_string(),
                serde_json::Value::String(block_sync_message_label(msg).to_string()),
            );
        });
    }

    fn trace_status_received(&self, status: BlockSyncStatus) {
        self.emit(bs_trace::BLOCK_STATUS_RECEIVED, |row| {
            bs_insert_height(row, "servable_low", status.servable_low);
            bs_insert_height(row, "servable_high", status.servable_high);
        });
    }

    fn trace_work_taken(&self, low: block::Height, high: block::Height, count: usize) {
        self.emit(bs_trace::BLOCK_WORK_TAKEN, |row| {
            bs_insert_height(row, "servable_low", low);
            bs_insert_height(row, "servable_high", high);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, count as u64);
        });
    }

    fn trace_get_blocks_sent(&self, start_height: block::Height, count: u32, estimated_bytes: u64) {
        self.emit(bs_trace::BLOCK_GET_BLOCKS_SENT, |row| {
            bs_insert_peer(row, bs_trace::PEER, &self.peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
            bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(count));
            bs_insert_u64(row, bs_trace::ESTIMATED_BYTES, estimated_bytes);
            bs_insert_u64(row, "available_slots", self.window.available_slots() as u64);
            bs_insert_u64(
                row,
                "peer_outstanding",
                self.window.outstanding.len() as u64,
            );
        });
    }

    fn trace_body_received(
        &self,
        height: block::Height,
        serialized_bytes: u64,
        request_start_height: Option<block::Height>,
        request_range_count: Option<u32>,
        request_elapsed_ms: Option<u64>,
    ) {
        self.emit(bs_trace::BLOCK_BODY_RECEIVED, |row| {
            bs_insert_peer(row, bs_trace::PEER, &self.peer);
            bs_insert_height(row, bs_trace::HEIGHT, height);
            bs_insert_u64(row, bs_trace::SERIALIZED_BYTES, serialized_bytes);
            bs_insert_u64(row, bs_trace::BUDGET_RESERVED_AFTER, self.budget.reserved());
            if let Some(request_start_height) = request_start_height {
                bs_insert_height(row, "request_start", request_start_height);
            }
            if let Some(request_range_count) = request_range_count {
                bs_insert_u64(row, "request_range_count", u64::from(request_range_count));
            }
            if let Some(request_elapsed_ms) = request_elapsed_ms {
                bs_insert_u64(row, "request_elapsed_ms", request_elapsed_ms);
            }
        });
    }

    fn trace_range_unavailable(
        &self,
        start_height: block::Height,
        range_count: Option<u32>,
        request_elapsed_ms: Option<u64>,
    ) {
        self.emit(bs_trace::BLOCK_RANGE_UNAVAILABLE, |row| {
            bs_insert_peer(row, bs_trace::PEER, &self.peer);
            bs_insert_height(row, bs_trace::RANGE_START, start_height);
            if let Some(range_count) = range_count {
                bs_insert_u64(row, bs_trace::RANGE_COUNT, u64::from(range_count));
            }
            if let Some(request_elapsed_ms) = request_elapsed_ms {
                bs_insert_u64(row, "request_elapsed_ms", request_elapsed_ms);
            }
        });
    }
}

fn elapsed_ms_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The still-unreceived heights of an outstanding request (the ones that return
/// to `pending` on retry/timeout — never the received-and-buffered ones, which
/// stay claimed in `work.in_flight`).
fn unreceived_heights(
    outstanding: &OutstandingBlockRange,
) -> impl Iterator<Item = block::Height> + '_ {
    outstanding
        .request
        .expected_hashes
        .iter()
        .filter(move |(height, _)| !outstanding.has_received(*height))
        .map(|(height, _)| *height)
}

impl Drop for PeerRoutine {
    /// §7.5 disconnect-mid-fetch correctness: on every exit path
    /// (cancel/panic/normal) return this routine's unreceived outstanding heights
    /// to `work.pending`, release their byte reservation, and clear this peer's
    /// outstanding set in the registry. All operations are sync (lock/atomic), so
    /// the guard is cancel-safe and panic-safe.
    ///
    /// The guard clears the peer's *outstanding* rather than removing the whole
    /// registry entry: a reset respawns the routine (the reactor cancels + spawns
    /// a fresh one) while the peer stays connected, so its servable/caps must
    /// survive. If the guard removed the entry, an old routine's async Drop could
    /// race *after* the respawned routine re-inserted and nuke the live entry.
    /// The reactor owns entry insert (on connect) and remove (on disconnect/
    /// admission-reject); see `handle_peer_disconnected`.
    fn drop(&mut self) {
        for outstanding in self.window.outstanding.drain(..) {
            self.budget.release(outstanding.reserved_bytes());
            self.work.return_items(
                outstanding
                    .request
                    .expected_hashes
                    .iter()
                    .filter(|(height, _)| !outstanding.has_received(*height))
                    .map(|(height, _)| *height),
            );
        }
        self.registry.clear_outstanding(&self.peer, self.generation);
    }
}
