//! Structured JSONL trace helpers for Zakura P2P.
//!
//! `closed.punitive` is reserved in the schema until Zakura has a punitive close
//! path and matching metric. `closed.neutral` carries a bounded `reason` label
//! describing the teardown cause (for example `idle_timeout`, `accept_failed`,
//! `outbound_closed`, `bad_response`, or `cancelled`). Connection admission
//! rejects use `rejected.admission` with a bounded `reason` label rather than
//! one event name per rejection metric, so readers should pivot by `event` plus
//! `reason` for those rows.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use blake2b_simd::Params as Blake2bParams;
use serde_json::{Map, Number, Value};
use zebra_jsonl_trace::{JsonlTracer, JsonlWriteEvent};

use super::{ZakuraPeerId, ZakuraRejectReason};

/// A Zakura JSONL trace table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZakuraTraceTable {
    table: &'static str,
    file_name: &'static str,
}

impl ZakuraTraceTable {
    /// Logical table name.
    pub fn table(self) -> &'static str {
        self.table
    }

    /// JSONL file name.
    pub fn file_name(self) -> &'static str {
        self.file_name
    }
}

/// Legacy upgrade and control-handshake transitions.
pub const HANDSHAKE_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "handshake",
    file_name: "handshake.jsonl",
};

/// Connection admission and close transitions.
pub const CONN_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "conn",
    file_name: "conn.jsonl",
};

/// Per-connection stream admission transitions.
pub const STREAM_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "stream",
    file_name: "stream.jsonl",
};

/// Discovery dialer decisions and backoff classification.
pub const DISCOVERY_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "discovery",
    file_name: "discovery.jsonl",
};

/// Frame and message rate-limit decisions.
pub const RATELIMIT_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "ratelimit",
    file_name: "ratelimit.jsonl",
};

/// Header-sync policy, accounting, and frontier events.
pub const HEADER_SYNC_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "header_sync",
    file_name: "header_sync.jsonl",
};

/// Legacy compatibility request/response events.
pub const LEGACY_REQUEST_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "legacy_request",
    file_name: "legacy_request.jsonl",
};

/// Block-sync (stream-6) scheduling, download, submit, and commit events.
pub const BLOCK_SYNC_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "block_sync",
    file_name: "block_sync.jsonl",
};

/// Zebrad adapter boundary events for commits, state reads, and frontier mirrors.
pub const COMMIT_STATE_TABLE: ZakuraTraceTable = ZakuraTraceTable {
    table: "commit_state",
    file_name: "commit_state.jsonl",
};

/// Shared block-sync trace event names and field keys.
///
/// The block-sync body pipeline has no `tracing`-macro coverage in release
/// builds (the binary is compiled with `release_max_level_info`, which strips
/// the `debug!` sites), so these JSONL rows are the only runtime visibility into
/// scheduling, download, submit, and commit progress. The periodic
/// [`BLOCK_SYNC_STATE`](block_sync_trace::BLOCK_SYNC_STATE) snapshot is the single most useful row for diagnosing a
/// stall: it reports where the body floor, verified tip, and header tip are, how
/// much is buffered/applying, and whether the byte budget or peer status is
/// blocking new downloads.
pub mod block_sync_trace {
    /// Trace row event field.
    pub const EVENT: &str = "event";
    /// Peer field.
    pub const PEER: &str = "peer";
    /// Action/event/message kind field.
    pub const KIND: &str = "kind";
    /// Height field.
    pub const HEIGHT: &str = "height";
    /// Hash field.
    pub const HASH: &str = "hash";
    /// Range start height field.
    pub const RANGE_START: &str = "range_start";
    /// Range count field.
    pub const RANGE_COUNT: &str = "range_count";
    /// Expected/requested count field.
    pub const EXPECTED_COUNT: &str = "expected_count";
    /// Estimated byte reservation for a requested range.
    pub const ESTIMATED_BYTES: &str = "estimated_bytes";
    /// Serialized byte size of a received body.
    pub const SERIALIZED_BYTES: &str = "serialized_bytes";
    /// Commit result label (`committed`, `duplicate`, `rejected`, `timed_out`).
    pub const RESULT: &str = "result";
    /// Reactor-local verifier submission token.
    pub const APPLY_TOKEN: &str = "apply_token";
    /// Bounded reason field.
    pub const REASON: &str = "reason";
    /// Highest contiguous body height already submitted for apply.
    pub const BODY_DOWNLOAD_FLOOR: &str = "body_download_floor";
    /// Highest verified (committed) block-body height.
    pub const VERIFIED_BLOCK_TIP: &str = "verified_block_tip";
    /// Best header tip driving the body-download target.
    pub const BEST_HEADER_TIP: &str = "best_header_tip";
    /// Header tip minus verified body tip.
    pub const BODY_LAG: &str = "body_lag";
    /// Count of blocks submitted-but-not-yet-committed (held against budget).
    pub const APPLYING: &str = "applying";
    /// Count of applying blocks already submitted to the verifier driver.
    pub const SUBMITTED_APPLIES: &str = "submitted_applies";
    /// Count of out-of-order bodies buffered awaiting a contiguous prefix.
    pub const REORDER: &str = "reorder";
    /// Count of outstanding (in-flight) range requests across peers.
    pub const OUTSTANDING: &str = "outstanding";
    /// Remaining in-flight body byte budget.
    pub const BUDGET_AVAILABLE: &str = "budget_available";
    /// Reserved in-flight body byte budget.
    pub const BUDGET_RESERVED: &str = "budget_reserved";
    /// Connected block-sync peers.
    pub const PEERS: &str = "peers";
    /// Connected block-sync peers whose status we have received (schedulable).
    pub const PEERS_WITH_STATUS: &str = "peers_with_status";
    /// Lowest height still in the body-sync `needed` set (the gap to fetch next).
    pub const NEEDED_MIN: &str = "needed_min";
    /// Number of heights in the body-sync `needed` set after buffer filtering.
    pub const NEEDED_COUNT: &str = "needed_count";
    /// Number of ranges queued in the scheduler.
    pub const QUEUE_LEN: &str = "queue_len";
    /// Number of block heights queued in the scheduler.
    pub const QUEUE_BLOCKS: &str = "queue_blocks";
    /// Lowest start height across queued scheduler ranges.
    pub const QUEUE_MIN_START: &str = "queue_min_start";
    /// Number of distinct assigned range keys in the scheduler.
    pub const ASSIGNED_LEN: &str = "assigned_len";
    /// Count of locally queued, in-flight, buffered, or applying block bodies.
    pub const LOCAL_BODY_WORK: &str = "local_body_work";
    /// Local body work threshold below which the reactor refills from state.
    pub const REFILL_LOW_WATER: &str = "refill_low_water";
    /// Highest end height across the scheduler's covered intervals.
    pub const COVERED_MAX_END: &str = "covered_max_end";

    /// Peer status received (servable body range advertised by the peer).
    pub const BLOCK_STATUS_RECEIVED: &str = "block_status_received";
    /// Local peer status queued for transport.
    pub const BLOCK_STATUS_SENT: &str = "block_status_sent";
    /// Local peer status failed to queue for transport.
    pub const BLOCK_STATUS_SEND_FAILED: &str = "block_status_send_failed";
    /// Block-sync peer connected to the reactor.
    pub const BLOCK_PEER_CONNECTED: &str = "block_peer_connected";
    /// Block-sync peer disconnected from the reactor.
    pub const BLOCK_PEER_DISCONNECTED: &str = "block_peer_disconnected";
    /// Body range request sent to a peer.
    pub const BLOCK_GET_BLOCKS_SENT: &str = "block_get_blocks_sent";
    /// Reactor accepted an inbound event.
    pub const BLOCK_EVENT_RECEIVED: &str = "block_event_received";
    /// Reactor queued an outbound driver action.
    pub const BLOCK_ACTION_DISPATCHED: &str = "block_action_dispatched";
    /// Body received from a peer.
    pub const BLOCK_BODY_RECEIVED: &str = "block_body_received";
    /// Body submitted to the verifier for commit.
    pub const BLOCK_BODY_SUBMITTED: &str = "block_body_submitted";
    /// Verifier finished applying a submitted body.
    pub const BLOCK_APPLY_FINISHED: &str = "block_apply_finished";
    /// Peer reported a requested range as unavailable.
    pub const BLOCK_RANGE_UNAVAILABLE: &str = "block_range_unavailable";
    /// Local node queued a block range response for transport.
    pub const BLOCK_RANGE_RESPONSE_SENT: &str = "block_range_response_sent";
    /// New body downloads were paused (lag, near-tip, or budget).
    pub const BLOCK_DOWNLOADS_PAUSED: &str = "block_downloads_paused";
    /// Scheduler could not issue another request for a peer with free slots.
    pub const BLOCK_SCHEDULE_SKIPPED: &str = "block_schedule_skipped";
    /// Verified body frontier advanced from state.
    pub const BLOCK_FRONTIERS_CHANGED: &str = "block_frontiers_changed";
    /// Chain tip reset rolled the body frontier back.
    pub const BLOCK_CHAIN_TIP_RESET: &str = "block_chain_tip_reset";
    /// Periodic reactor state snapshot (the key stall-diagnosis row).
    pub const BLOCK_SYNC_STATE: &str = "block_sync_state";
}

/// Shared discovery trace event names and field keys.
pub mod discovery_trace {
    /// Trace row event field.
    pub const EVENT: &str = "event";
    /// Peer field.
    pub const PEER: &str = "peer";
    /// Dial result label.
    pub const RESULT: &str = "result";

    /// A discovery dial worker completed and was classified for backoff.
    pub const DISCOVERY_DIAL_RESULT: &str = "discovery_dial_result";
}

/// Shared header-sync trace event names and field keys.
pub mod header_sync_trace {
    /// Trace row event field.
    pub const EVENT: &str = "event";
    /// Peer field.
    pub const PEER: &str = "peer";
    /// Action/event/message kind field.
    pub const KIND: &str = "kind";
    /// Source peer field for forwarded full-block floods.
    pub const SOURCE_PEER: &str = "source_peer";
    /// Height field.
    pub const HEIGHT: &str = "height";
    /// Hash field.
    pub const HASH: &str = "hash";
    /// Header anchor hash field.
    pub const ANCHOR_HASH: &str = "anchor_hash";
    /// Range start height field.
    pub const RANGE_START: &str = "range_start";
    /// Range count field.
    pub const RANGE_COUNT: &str = "range_count";
    /// Header validation stage field.
    pub const VALIDATION_STAGE: &str = "validation_stage";
    /// Concrete validation error kind field.
    pub const ERROR_KIND: &str = "error_kind";
    /// Advertised peer range cap field.
    pub const ADVERTISED_CAP: &str = "advertised_cap";
    /// Expected header count field.
    pub const EXPECTED_COUNT: &str = "expected_count";
    /// In-flight request count field.
    pub const IN_FLIGHT_COUNT: &str = "in_flight_count";
    /// Destination peer count field.
    pub const DESTINATION_PEER_COUNT: &str = "destination_peer_count";
    /// Bounded reason field.
    pub const REASON: &str = "reason";

    /// Reactor accepted an inbound event.
    pub const HEADER_EVENT_RECEIVED: &str = "header_event_received";
    /// Reactor queued an outbound driver action.
    pub const HEADER_ACTION_DISPATCHED: &str = "header_action_dispatched";
    /// Local status sent to a peer.
    pub const HEADER_STATUS_SENT: &str = "header_status_sent";
    /// Peer status received.
    pub const HEADER_STATUS_RECEIVED: &str = "header_status_received";
    /// Header range request sent.
    pub const HEADER_GET_HEADERS_SENT: &str = "header_get_headers_sent";
    /// Header range response received.
    pub const HEADER_HEADERS_RECEIVED: &str = "header_headers_received";
    /// Header range response served from local state.
    pub const HEADER_HEADERS_SERVED: &str = "header_headers_served";
    /// Header range committed.
    pub const HEADER_RANGE_COMMITTED: &str = "header_range_committed";
    /// Header range rejected.
    pub const HEADER_RANGE_REJECTED: &str = "header_range_rejected";
    /// NewBlock tip flood received.
    pub const HEADER_NEW_BLOCK_RECEIVED: &str = "header_new_block_received";
    /// NewBlock tip flood forwarded.
    pub const HEADER_NEW_BLOCK_FORWARDED: &str = "header_new_block_forwarded";
    /// NewBlock tip flood deduped.
    pub const HEADER_NEW_BLOCK_DEDUPED: &str = "header_new_block_deduped";
    /// Peer violation observed.
    pub const HEADER_PEER_VIOLATION: &str = "header_peer_violation";
    /// Peer disconnect requested.
    pub const HEADER_PEER_DISCONNECT_REQUESTED: &str = "header_peer_disconnect_requested";
    /// Header frontier advanced.
    pub const HEADER_FRONTIER_ADVANCED: &str = "header_frontier_advanced";
    /// Header frontier re-anchored down to the verified block frontier.
    pub const HEADER_FRONTIER_REANCHORED: &str = "header_frontier_reanchored";
    /// Missing block bodies reported.
    pub const HEADER_MISSING_BODIES_REPORTED: &str = "header_missing_bodies_reported";
}

/// Shared commit/frontier adapter trace event names and field keys.
pub mod commit_state_trace {
    /// Trace row event field.
    pub const EVENT: &str = "event";
    /// Source driver/subsystem field.
    pub const SOURCE: &str = "source";
    /// Height field.
    pub const HEIGHT: &str = "height";
    /// Hash field.
    pub const HASH: &str = "hash";
    /// Range start height field.
    pub const RANGE_START: &str = "range_start";
    /// Range count field.
    pub const RANGE_COUNT: &str = "range_count";
    /// Result label field.
    pub const RESULT: &str = "result";
    /// Bounded reason field.
    pub const REASON: &str = "reason";
    /// Reactor-local block apply token field.
    pub const APPLY_TOKEN: &str = "apply_token";
    /// Apply class field (`checkpoint` or `full`).
    pub const APPLY_CLASS: &str = "apply_class";
    /// Finalized height observed from state.
    pub const FINALIZED_HEIGHT: &str = "finalized_height";
    /// Verified full-block/body tip height.
    pub const VERIFIED_BLOCK_TIP: &str = "verified_block_tip";
    /// Verified full-block/body tip hash.
    pub const VERIFIED_BLOCK_HASH: &str = "verified_block_hash";
    /// Best header tip height.
    pub const BEST_HEADER_TIP: &str = "best_header_tip";
    /// Elapsed milliseconds field.
    pub const ELAPSED_MS: &str = "elapsed_ms";
    /// Peer field.
    pub const PEER: &str = "peer";
    /// Queue length field.
    pub const QUEUE_LEN: &str = "queue_len";
    /// In-flight count field.
    pub const IN_FLIGHT_COUNT: &str = "in_flight_count";
    /// Action kind field.
    pub const ACTION: &str = "action";
    /// Whether an optional frontier was present.
    pub const LOCAL_FRONTIER: &str = "local_frontier";

    /// Driver received a reactor action.
    pub const ACTION_RECEIVED: &str = "action_received";
    /// State read started.
    pub const STATE_READ_START: &str = "state_read_start";
    /// State read completed successfully.
    pub const STATE_READ_SUCCESS: &str = "state_read_success";
    /// State read failed or returned an unexpected response.
    pub const STATE_READ_ERROR: &str = "state_read_error";
    /// State read timed out.
    pub const STATE_READ_TIMEOUT: &str = "state_read_timeout";
    /// Block submit was queued in the driver.
    pub const BLOCK_SUBMIT_QUEUED: &str = "block_submit_queued";
    /// Verifier commit started.
    pub const COMMIT_START: &str = "commit_start";
    /// Verifier commit exceeded the driver timeout but is still being awaited.
    pub const COMMIT_STALLED: &str = "commit_stalled";
    /// Verifier commit finished.
    pub const COMMIT_FINISH: &str = "commit_finish";
    /// Post-commit frontier query started.
    pub const FRONTIER_QUERY_START: &str = "frontier_query_start";
    /// Post-commit frontier query finished.
    pub const FRONTIER_QUERY_FINISH: &str = "frontier_query_finish";
    /// Driver sent an event back to a reactor.
    pub const REACTOR_EVENT_SENT: &str = "reactor_event_sent";
    /// Delayed checkpoint frontier refresh attempted.
    pub const CHECKPOINT_REFRESH_ATTEMPT: &str = "checkpoint_refresh_attempt";
    /// Delayed checkpoint frontier refresh sent a frontier event.
    pub const CHECKPOINT_REFRESH_SENT: &str = "checkpoint_refresh_sent";
    /// Header-sync driver notified block sync about a header tip.
    pub const BLOCK_SYNC_NOTIFY_SENT: &str = "block_sync_notify_sent";
    /// Chain-tip mirror observed a watch action.
    pub const CHAIN_TIP_ACTION: &str = "chain_tip_action";
    /// Chain-tip mirror derived local frontiers.
    pub const FRONTIER_DERIVED: &str = "frontier_derived";
    /// Shared sync exchange accepted or ignored a frontier update.
    pub const SYNC_FRONTIER_TRANSITION: &str = "sync_frontier_transition";
    /// Monotonic shared sync exchange transition sequence.
    pub const SEQUENCE: &str = "sequence";
    /// Shared sync exchange transition cause.
    pub const CAUSE: &str = "cause";
    /// Previous finalized frontier height.
    pub const OLD_FINALIZED_HEIGHT: &str = "old_finalized_height";
    /// Previous finalized frontier hash.
    pub const OLD_FINALIZED_HASH: &str = "old_finalized_hash";
    /// Previous verified body frontier height.
    pub const OLD_VERIFIED_BODY_HEIGHT: &str = "old_verified_body_height";
    /// Previous verified body frontier hash.
    pub const OLD_VERIFIED_BODY_HASH: &str = "old_verified_body_hash";
    /// Previous best header frontier height.
    pub const OLD_BEST_HEADER_HEIGHT: &str = "old_best_header_height";
    /// Previous best header frontier hash.
    pub const OLD_BEST_HEADER_HASH: &str = "old_best_header_hash";
    /// New finalized frontier height.
    pub const NEW_FINALIZED_HEIGHT: &str = "new_finalized_height";
    /// New finalized frontier hash.
    pub const NEW_FINALIZED_HASH: &str = "new_finalized_hash";
    /// New verified body frontier height.
    pub const NEW_VERIFIED_BODY_HEIGHT: &str = "new_verified_body_height";
    /// New verified body frontier hash.
    pub const NEW_VERIFIED_BODY_HASH: &str = "new_verified_body_hash";
    /// New best header frontier height.
    pub const NEW_BEST_HEADER_HEIGHT: &str = "new_best_header_height";
    /// New best header frontier hash.
    pub const NEW_BEST_HEADER_HASH: &str = "new_best_header_hash";
}

/// Cloneable Zakura trace emitter.
#[derive(Clone, Debug)]
pub struct ZakuraTrace {
    tracer: JsonlTracer,
    node: Arc<str>,
    started: Instant,
}

impl ZakuraTrace {
    /// Create a no-op trace emitter.
    pub fn noop() -> Self {
        Self::new(JsonlTracer::noop(), zebra_jsonl_trace::node_id())
    }

    /// Create a trace emitter with an explicit node label.
    pub fn new(tracer: JsonlTracer, node: impl Into<Arc<str>>) -> Self {
        Self {
            tracer,
            node: node.into(),
            started: Instant::now(),
        }
    }

    /// Return the underlying JSONL tracer.
    pub fn tracer(&self) -> &JsonlTracer {
        &self.tracer
    }

    /// Return true when this emitter will attempt to write rows.
    pub fn is_enabled(&self) -> bool {
        self.tracer.is_enabled()
    }

    /// Emit one event row without awaiting or back-pressuring the caller.
    pub fn emit(&self, table: ZakuraTraceTable, event: ZakuraTraceEvent<'_>) {
        self.emit_with(table, |row| event.insert_into(row));
    }

    /// Emit one event row, building the row only when a queue slot is reserved.
    ///
    /// Reserving the bounded channel slot first means the `build` closure and
    /// serialization never run when the queue is full or the writer has closed,
    /// keeping attacker-rate emit sites cheap once the writer falls behind.
    pub fn emit_with(&self, table: ZakuraTraceTable, build: impl FnOnce(&mut Map<String, Value>)) {
        let Ok(permit) = self.tracer.try_reserve() else {
            return;
        };

        let mut row = Map::new();
        row.insert("ts".to_string(), elapsed_micros(self.started.elapsed()));
        row.insert("node".to_string(), Value::String(self.node.to_string()));
        build(&mut row);

        if let Ok(line) = serde_json::to_vec(&Value::Object(row)) {
            permit.send(JsonlWriteEvent {
                table: table.table(),
                file_name: table.file_name(),
                line,
            });
        }
    }
}

impl Default for ZakuraTrace {
    fn default() -> Self {
        Self::noop()
    }
}

/// A single Zakura trace event before serialization.
#[derive(Clone, Debug)]
pub struct ZakuraTraceEvent<'a> {
    event: &'static str,
    conn: Option<u64>,
    stream: Option<u64>,
    payload_len: Option<u64>,
    frame_len: Option<u64>,
    max_frame_bytes: Option<u64>,
    peer: Option<&'a str>,
    role: Option<&'static str>,
    phase: Option<&'static str>,
    reason: Option<&'static str>,
    selected_protocol: Option<u16>,
    direction: Option<&'static str>,
    stream_kind: Option<&'static str>,
    network: Option<&'static str>,
}

impl<'a> ZakuraTraceEvent<'a> {
    /// Create an event row with the required dotted event name.
    pub fn new(event: &'static str) -> Self {
        Self {
            event,
            conn: None,
            stream: None,
            payload_len: None,
            frame_len: None,
            max_frame_bytes: None,
            peer: None,
            role: None,
            phase: None,
            reason: None,
            selected_protocol: None,
            direction: None,
            stream_kind: None,
            network: None,
        }
    }

    /// Attach a local connection id.
    pub fn conn(mut self, conn: u64) -> Self {
        self.conn = Some(conn);
        self
    }

    /// Attach a local stream id.
    pub fn stream(mut self, stream: u64) -> Self {
        self.stream = Some(stream);
        self
    }

    /// Attach a declared frame payload length.
    pub fn payload_len(mut self, payload_len: u64) -> Self {
        self.payload_len = Some(payload_len);
        self
    }

    /// Attach an encoded frame length.
    pub fn frame_len(mut self, frame_len: u64) -> Self {
        self.frame_len = Some(frame_len);
        self
    }

    /// Attach the effective frame byte cap used by the receiver.
    pub fn max_frame_bytes(mut self, max_frame_bytes: u64) -> Self {
        self.max_frame_bytes = Some(max_frame_bytes);
        self
    }

    /// Attach a bounded peer label.
    pub fn peer(mut self, peer: &'a str) -> Self {
        self.peer = Some(peer);
        self
    }

    /// Attach a bounded peer label when one is available.
    pub fn maybe_peer(mut self, peer: Option<&'a str>) -> Self {
        self.peer = peer;
        self
    }

    /// Attach a control role label.
    pub fn role(mut self, role: &'static str) -> Self {
        self.role = Some(role);
        self
    }

    /// Attach a handshake phase label.
    pub fn phase(mut self, phase: &'static str) -> Self {
        self.phase = Some(phase);
        self
    }

    /// Attach a bounded reason label.
    pub fn reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Attach the selected Zakura protocol version.
    pub fn selected_protocol(mut self, selected_protocol: u16) -> Self {
        self.selected_protocol = Some(selected_protocol);
        self
    }

    /// Attach a connection direction label.
    pub fn direction(mut self, direction: &'static str) -> Self {
        self.direction = Some(direction);
        self
    }

    /// Attach a stream kind label.
    pub fn stream_kind(mut self, stream_kind: &'static str) -> Self {
        self.stream_kind = Some(stream_kind);
        self
    }

    /// Attach a network label.
    pub fn network(mut self, network: &'static str) -> Self {
        self.network = Some(network);
        self
    }

    fn insert_into(self, row: &mut Map<String, Value>) {
        row.insert("event".to_string(), Value::String(self.event.to_string()));
        insert_optional_u64(row, "conn", self.conn);
        insert_optional_u64(row, "stream", self.stream);
        insert_optional_u64(row, "payload_len", self.payload_len);
        insert_optional_u64(row, "frame_len", self.frame_len);
        insert_optional_u64(row, "max_frame_bytes", self.max_frame_bytes);
        insert_optional_str(row, "peer", self.peer);
        insert_optional_str(row, "role", self.role);
        insert_optional_str(row, "phase", self.phase);
        insert_optional_str(row, "reason", self.reason);
        insert_optional_u64(
            row,
            "selected_protocol",
            self.selected_protocol.map(u64::from),
        );
        insert_optional_str(row, "direction", self.direction);
        insert_optional_str(row, "stream_kind", self.stream_kind);
        insert_optional_str(row, "network", self.network);
    }
}

/// Return a stable, bounded label for a peer id.
pub fn peer_label(peer_id: &ZakuraPeerId) -> String {
    let hash = Blake2bParams::new()
        .hash_length(8)
        .personal(b"zakura-peer-lbl")
        .hash(peer_id.as_bytes());
    format!("peer:{}", hex::encode(hash.as_bytes()))
}

/// Return the low-cardinality trace label for a rejection reason.
pub fn reject_reason_label(reason: ZakuraRejectReason) -> &'static str {
    match reason {
        ZakuraRejectReason::UnsupportedPreludeVersion => "unsupported_prelude_version",
        ZakuraRejectReason::IncompatibleZakuraProtocol => "incompatible_zakura_protocol",
        ZakuraRejectReason::WrongNetwork => "wrong_network",
        ZakuraRejectReason::WrongChain => "wrong_chain",
        ZakuraRejectReason::MissingRequiredCapability => "missing_required_capability",
        ZakuraRejectReason::ResourceLimit => "resource_limit",
        ZakuraRejectReason::AlreadyConnected => "already_connected",
        ZakuraRejectReason::TemporaryUnavailable => "temporary_unavailable",
    }
}

fn elapsed_micros(elapsed: Duration) -> Value {
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    Value::Number(Number::from(micros))
}

fn insert_optional_str(row: &mut Map<String, Value>, key: &'static str, value: Option<&str>) {
    row.insert(
        key.to_string(),
        value.map_or(Value::Null, |value| Value::String(value.to_string())),
    );
}

fn insert_optional_u64(row: &mut Map<String, Value>, key: &'static str, value: Option<u64>) {
    row.insert(
        key.to_string(),
        value.map_or(Value::Null, |value| Value::Number(Number::from(value))),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn noop_trace_does_not_build_rows() {
        let trace = ZakuraTrace::noop();
        let called = Arc::new(AtomicBool::new(false));
        let called_in_emit = called.clone();

        trace.emit_with(CONN_TABLE, |_| {
            called_in_emit.store(true, Ordering::SeqCst);
        });

        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn full_queue_does_not_build_rows() {
        // Capacity 1, pre-filled so the next reserve fails with `Full`.
        let (tx, _rx) = mpsc::channel(1);
        let tracer = JsonlTracer::new(tx);
        let trace = ZakuraTrace::new(tracer, "node-full");

        trace.emit(CONN_TABLE, ZakuraTraceEvent::new("conn.fill"));

        let called = Arc::new(AtomicBool::new(false));
        let called_in_emit = called.clone();
        trace.emit_with(CONN_TABLE, |_| {
            called_in_emit.store(true, Ordering::SeqCst);
        });

        assert!(
            !called.load(Ordering::SeqCst),
            "build closure must not run when the queue is full"
        );
    }

    #[test]
    fn closed_queue_does_not_build_rows() {
        // Dropping the receiver closes the channel; reserve fails with `Closed`.
        let (tx, rx) = mpsc::channel(1);
        let tracer = JsonlTracer::new(tx);
        let trace = ZakuraTrace::new(tracer, "node-closed");
        drop(rx);

        let called = Arc::new(AtomicBool::new(false));
        let called_in_emit = called.clone();
        trace.emit_with(CONN_TABLE, |_| {
            called_in_emit.store(true, Ordering::SeqCst);
        });

        assert!(
            !called.load(Ordering::SeqCst),
            "build closure must not run when the queue is closed"
        );
        assert!(
            !trace.is_enabled(),
            "trace must report disabled once the receiver is dropped"
        );
    }
}
