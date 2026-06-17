//! Structured JSONL trace helpers for Zakura P2P.
//!
//! `closed.punitive` is reserved in the schema until Zakura has a punitive close
//! path and matching metric. Connection admission rejects use
//! `rejected.admission` with a bounded `reason` label rather than one event name
//! per rejection metric, so readers should pivot by `event` plus `reason` for
//! those rows.

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

/// Shared header-sync trace event names and field keys.
pub mod header_sync_trace {
    /// Trace row event field.
    pub const EVENT: &str = "event";
    /// Peer field.
    pub const PEER: &str = "peer";
    /// Source peer field for forwarded full-block floods.
    pub const SOURCE_PEER: &str = "source_peer";
    /// Height field.
    pub const HEIGHT: &str = "height";
    /// Hash field.
    pub const HASH: &str = "hash";
    /// Range start height field.
    pub const RANGE_START: &str = "range_start";
    /// Range count field.
    pub const RANGE_COUNT: &str = "range_count";
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
    /// Missing block bodies reported.
    pub const HEADER_MISSING_BODIES_REPORTED: &str = "header_missing_bodies_reported";
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
