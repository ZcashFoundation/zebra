//! Shared non-blocking JSONL tracing support for Zebra components.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::OnceLock,
    time::Duration,
};

use tokio::{
    io::AsyncWriteExt,
    runtime::Handle,
    sync::mpsc::{self, error::TryRecvError, error::TrySendError},
    time::{self, Instant, MissedTickBehavior},
};

/// Default trace channel capacity.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 16_384;

/// Default maximum number of events to drain into a single write batch.
pub const DEFAULT_MAX_BATCH_EVENTS: usize = 256;

/// Default amount of time the writer waits for more events after receiving the
/// first event in a batch.
pub const DEFAULT_BATCH_LINGER: Duration = Duration::from_millis(5);

/// Default number of buffered bytes before the writer flushes to the file.
pub const DEFAULT_BUFFER_FLUSH_BYTES: usize = 256 * 1024;

/// Default interval between forced file flushes and syncs.
pub const DEFAULT_FILE_FLUSH_INTERVAL: Duration = Duration::from_secs(17);

/// Env var used to label every JSONL trace record with a stable node identifier.
pub const NODE_ID_ENV: &str = "ZEBRA_NODE_ID";

/// Returns the process-wide node identifier used to tag JSONL trace records.
///
/// Resolution order: `ZEBRA_NODE_ID`, then `HOSTNAME`, then `"unknown"`. The
/// value is resolved once on first call and cached for the lifetime of the
/// process so every trace record from this node reports the same id.
pub fn node_id() -> &'static str {
    static NODE_ID: OnceLock<String> = OnceLock::new();
    NODE_ID
        .get_or_init(|| {
            std::env::var(NODE_ID_ENV)
                .ok()
                .or_else(|| std::env::var("HOSTNAME").ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .as_str()
}

/// A pre-serialized JSONL record to be written to a per-table file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlWriteEvent {
    /// Logical table name used for diagnostics.
    pub table: &'static str,
    /// Output file name for this table.
    pub file_name: &'static str,
    /// Pre-serialized JSON bytes for a single record, without a trailing newline.
    pub line: Vec<u8>,
}

/// Settings for the background JSONL writer.
#[derive(Clone, Debug)]
pub struct JsonlTraceConfig {
    /// Bounded queue capacity.
    pub channel_capacity: usize,
    /// Maximum number of events to write in a single batch.
    pub max_batch_events: usize,
    /// How long to wait for more events after receiving the first batch event.
    pub batch_linger: Duration,
    /// Buffered bytes threshold before flushing to the underlying file.
    pub buffer_flush_bytes: usize,
    /// Maximum time between forced file flushes and syncs.
    pub file_flush_interval: Duration,
}

impl Default for JsonlTraceConfig {
    fn default() -> Self {
        Self {
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            max_batch_events: DEFAULT_MAX_BATCH_EVENTS,
            batch_linger: DEFAULT_BATCH_LINGER,
            buffer_flush_bytes: DEFAULT_BUFFER_FLUSH_BYTES,
            file_flush_interval: DEFAULT_FILE_FLUSH_INTERVAL,
        }
    }
}

#[derive(Clone)]
struct TraceRuntime {
    tx: mpsc::Sender<JsonlWriteEvent>,
}

/// A non-blocking handle for emitting JSONL trace records.
#[derive(Clone)]
pub struct JsonlTracer {
    inner: TraceState,
}

#[derive(Clone)]
enum TraceState {
    Disabled,
    Enabled(TraceRuntime),
}

/// Reserve errors for the bounded JSONL trace queue.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JsonlTraceReserveError {
    /// Tracing is disabled.
    Disabled,
    /// The queue is full.
    Full,
    /// The writer task has closed.
    Closed,
}

/// Send errors for the bounded JSONL trace queue.
#[derive(Debug)]
pub enum JsonlTraceSendError {
    /// Tracing is disabled.
    Disabled(JsonlWriteEvent),
    /// The queue is full.
    Full(JsonlWriteEvent),
    /// The writer task has closed.
    Closed(JsonlWriteEvent),
}

/// A reserved queue slot for a trace record.
#[derive(Debug)]
pub struct JsonlTracePermit {
    permit: mpsc::OwnedPermit<JsonlWriteEvent>,
}

impl JsonlTracePermit {
    /// Send a record into the reserved queue slot.
    pub fn send(self, event: JsonlWriteEvent) {
        self.permit.send(event);
    }
}

impl JsonlTracer {
    /// Create a tracer backed by the supplied sender.
    pub fn new(tx: mpsc::Sender<JsonlWriteEvent>) -> Self {
        Self {
            inner: TraceState::Enabled(TraceRuntime { tx }),
        }
    }

    /// Create a no-op tracer.
    pub fn noop() -> Self {
        Self {
            inner: TraceState::Disabled,
        }
    }

    /// Spawn a background writer on the current Tokio runtime.
    ///
    /// If there is no current Tokio runtime, tracing is disabled and a no-op
    /// tracer is returned.
    pub fn spawn(trace_dir: PathBuf) -> Self {
        Self::spawn_with_config(trace_dir, JsonlTraceConfig::default())
    }

    /// Spawn a background writer using `config` on the current Tokio runtime.
    ///
    /// If there is no current Tokio runtime, tracing is disabled and a no-op
    /// tracer is returned.
    pub fn spawn_with_config(trace_dir: PathBuf, config: JsonlTraceConfig) -> Self {
        let Ok(handle) = Handle::try_current() else {
            tracing::warn!(
                ?trace_dir,
                "JSONL tracing requested without an active Tokio runtime, disabling tracing"
            );
            return Self::noop();
        };

        let (tx, rx) = mpsc::channel(config.channel_capacity);
        let writer = TraceWriter::new(trace_dir.clone(), config);
        handle.spawn(run_trace_writer(rx, writer));
        tracing::info!(?trace_dir, "JSONL tracing enabled");

        Self::new(tx)
    }

    /// Returns `true` if this tracer will emit records.
    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, TraceState::Enabled(_))
    }

    /// Returns the remaining queue capacity.
    pub fn capacity(&self) -> usize {
        let TraceState::Enabled(runtime) = &self.inner else {
            return 0;
        };

        runtime.tx.capacity()
    }

    /// Try to reserve a queue slot for a trace record.
    pub fn try_reserve(&self) -> Result<JsonlTracePermit, JsonlTraceReserveError> {
        let TraceState::Enabled(runtime) = &self.inner else {
            return Err(JsonlTraceReserveError::Disabled);
        };

        runtime
            .tx
            .clone()
            .try_reserve_owned()
            .map(|permit| JsonlTracePermit { permit })
            .map_err(|error| match error {
                TrySendError::Full(_) => JsonlTraceReserveError::Full,
                TrySendError::Closed(_) => JsonlTraceReserveError::Closed,
            })
    }

    /// Try to send a trace record without blocking.
    pub fn try_send(&self, event: JsonlWriteEvent) -> Result<(), JsonlTraceSendError> {
        let TraceState::Enabled(runtime) = &self.inner else {
            return Err(JsonlTraceSendError::Disabled(event));
        };

        runtime.tx.try_send(event).map_err(|error| match error {
            TrySendError::Full(event) => JsonlTraceSendError::Full(event),
            TrySendError::Closed(event) => JsonlTraceSendError::Closed(event),
        })
    }
}

impl std::fmt::Debug for JsonlTracer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlTracer").finish()
    }
}

struct TableWriter {
    file: tokio::fs::File,
    encode_buf: Vec<u8>,
}

impl TableWriter {
    fn new(file: tokio::fs::File, buffer_flush_bytes: usize) -> Self {
        Self {
            file,
            encode_buf: Vec::with_capacity(buffer_flush_bytes),
        }
    }

    fn append_line(&mut self, line: &[u8]) {
        self.encode_buf.extend_from_slice(line);
        self.encode_buf.push(b'\n');
    }

    async fn flush_buffer(&mut self, sync_file: bool) -> std::io::Result<()> {
        if !self.encode_buf.is_empty() {
            self.file.write_all(&self.encode_buf).await?;
            self.encode_buf.clear();
            self.file.flush().await?;
        }

        if sync_file {
            self.file.sync_data().await?;
        }

        Ok(())
    }
}

struct TraceWriter {
    trace_dir: PathBuf,
    config: JsonlTraceConfig,
    tables: HashMap<&'static str, TableWriter>,
    disabled_tables: HashSet<&'static str>,
    trace_dir_created: bool,
    last_file_flush: Instant,
}

impl TraceWriter {
    fn new(trace_dir: PathBuf, config: JsonlTraceConfig) -> Self {
        Self {
            trace_dir,
            config,
            tables: HashMap::new(),
            disabled_tables: HashSet::new(),
            trace_dir_created: false,
            last_file_flush: Instant::now(),
        }
    }

    fn has_open_files(&self) -> bool {
        !self.tables.is_empty() || self.disabled_tables.is_empty()
    }

    async fn write_batch(&mut self, batch: Vec<JsonlWriteEvent>, force_flush: bool) {
        for event in batch {
            if self.disabled_tables.contains(event.table) {
                continue;
            }

            let append_result = match self.table_writer_mut(event.table, event.file_name).await {
                Some(table_writer) => {
                    table_writer.append_line(&event.line);
                    Ok(())
                }
                None => Err(()),
            };

            if append_result.is_err() {
                self.disable_table(event.table);
            }
        }

        let flush_file =
            force_flush || self.last_file_flush.elapsed() >= self.config.file_flush_interval;
        let mut failed_tables = Vec::new();

        for (&table_name, table_writer) in &mut self.tables {
            let should_flush_buffer =
                table_writer.encode_buf.len() >= self.config.buffer_flush_bytes;

            if !should_flush_buffer && !flush_file && !force_flush {
                continue;
            }

            if let Err(error) = table_writer.flush_buffer(flush_file || force_flush).await {
                tracing::warn!(
                    ?error,
                    table = table_name,
                    trace_dir = ?self.trace_dir,
                    "disabling trace table after write failure"
                );
                failed_tables.push(table_name);
            }
        }

        if flush_file || force_flush {
            self.last_file_flush = Instant::now();
        }

        for table in failed_tables {
            self.disable_table(table);
        }
    }

    async fn flush_all(&mut self) {
        self.write_batch(Vec::new(), true).await;
    }

    fn disable_table(&mut self, table: &'static str) {
        self.tables.remove(table);
        self.disabled_tables.insert(table);
    }

    async fn table_writer_mut(
        &mut self,
        table: &'static str,
        file_name: &'static str,
    ) -> Option<&mut TableWriter> {
        if self.disabled_tables.contains(table) {
            return None;
        }

        if !self.tables.contains_key(table) {
            if !self.trace_dir_created {
                if let Err(error) = tokio::fs::create_dir_all(&self.trace_dir).await {
                    tracing::warn!(
                        ?error,
                        trace_dir = ?self.trace_dir,
                        "failed to create trace directory, disabling trace table"
                    );
                    self.disabled_tables.insert(table);
                    return None;
                }
                self.trace_dir_created = true;
            }

            let file = match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.trace_dir.join(file_name))
                .await
            {
                Ok(file) => file,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        table,
                        trace_dir = ?self.trace_dir,
                        "failed to open trace table, disabling trace table"
                    );
                    self.disabled_tables.insert(table);
                    return None;
                }
            };

            self.tables.insert(
                table,
                TableWriter::new(file, self.config.buffer_flush_bytes),
            );
        }

        self.tables.get_mut(table)
    }
}

async fn run_trace_writer(mut rx: mpsc::Receiver<JsonlWriteEvent>, mut writer: TraceWriter) {
    let mut flush_tick = time::interval(writer.config.file_flush_interval);
    flush_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let mut batch = Vec::with_capacity(writer.config.max_batch_events);
        let mut receiver_closed = false;
        let mut force_flush = false;

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => batch.push(event),
                    None => receiver_closed = true,
                }
            }
            _ = flush_tick.tick(), if writer.has_open_files() => {
                writer.flush_all().await;

                if !writer.has_open_files() {
                    tracing::warn!(trace_dir = ?writer.trace_dir, "all trace tables have been disabled");
                    break;
                }

                continue;
            }
        }

        if receiver_closed {
            writer.flush_all().await;
            break;
        }

        let deadline = Instant::now() + writer.config.batch_linger;
        let sleep = time::sleep_until(deadline);
        tokio::pin!(sleep);

        while batch.len() < writer.config.max_batch_events {
            match rx.try_recv() {
                Ok(event) => {
                    batch.push(event);
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    receiver_closed = true;
                    break;
                }
                Err(TryRecvError::Empty) => {}
            }

            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => batch.push(event),
                        None => {
                            receiver_closed = true;
                            break;
                        }
                    }
                }
                _ = flush_tick.tick(), if writer.has_open_files() => {
                    force_flush = true;
                    break;
                }
                _ = &mut sleep => break,
            }
        }

        writer
            .write_batch(batch, force_flush || receiver_closed)
            .await;

        if !writer.has_open_files() {
            tracing::warn!(trace_dir = ?writer.trace_dir, "all trace tables have been disabled");
            break;
        }

        if receiver_closed {
            writer.flush_all().await;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_task_produces_per_table_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace_dir = dir.path().join("traces");

        let (tx, rx) = mpsc::channel(16);
        let writer = TraceWriter::new(trace_dir.clone(), JsonlTraceConfig::default());
        let handle = tokio::spawn(run_trace_writer(rx, writer));

        tx.send(JsonlWriteEvent {
            table: "alpha",
            file_name: "alpha.jsonl",
            line: br#"{"value":1}"#.to_vec(),
        })
        .await
        .expect("send should succeed");

        tx.send(JsonlWriteEvent {
            table: "beta",
            file_name: "beta.jsonl",
            line: br#"{"value":2}"#.to_vec(),
        })
        .await
        .expect("send should succeed");

        drop(tx);
        handle.await.expect("writer task should complete");

        let alpha = tokio::fs::read_to_string(trace_dir.join("alpha.jsonl"))
            .await
            .expect("alpha file");
        let beta = tokio::fs::read_to_string(trace_dir.join("beta.jsonl"))
            .await
            .expect("beta file");

        assert_eq!(alpha.trim(), "{\"value\":1}");
        assert_eq!(beta.trim(), "{\"value\":2}");
    }

    #[tokio::test]
    async fn writer_flushes_idle_buffers_on_timer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace_dir = dir.path().join("traces");

        let config = JsonlTraceConfig {
            batch_linger: Duration::from_millis(1),
            buffer_flush_bytes: 1024,
            file_flush_interval: Duration::from_millis(25),
            ..JsonlTraceConfig::default()
        };

        let (tx, rx) = mpsc::channel(16);
        let writer = TraceWriter::new(trace_dir.clone(), config);
        let handle = tokio::spawn(run_trace_writer(rx, writer));

        tx.send(JsonlWriteEvent {
            table: "alpha",
            file_name: "alpha.jsonl",
            line: br#"{"value":1}"#.to_vec(),
        })
        .await
        .expect("send should succeed");

        time::sleep(Duration::from_millis(80)).await;

        let alpha = tokio::fs::read_to_string(trace_dir.join("alpha.jsonl"))
            .await
            .expect("alpha file should be flushed while the writer is idle");

        assert_eq!(alpha.trim(), "{\"value\":1}");

        drop(tx);
        handle.await.expect("writer task should complete");
    }

    #[test]
    fn noop_tracer_returns_disabled_errors() {
        let tracer = JsonlTracer::noop();

        assert!(matches!(
            tracer.try_reserve(),
            Err(JsonlTraceReserveError::Disabled)
        ));

        let send_result = tracer.try_send(JsonlWriteEvent {
            table: "alpha",
            file_name: "alpha.jsonl",
            line: br#"{"value":1}"#.to_vec(),
        });

        assert!(matches!(send_result, Err(JsonlTraceSendError::Disabled(_))));
    }
}
