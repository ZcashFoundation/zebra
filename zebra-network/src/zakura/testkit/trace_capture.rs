//! Capture-and-discard JSONL traces for Zakura tests.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use zebra_jsonl_trace::{JsonlTraceConfig, JsonlTraceGuard, JsonlTracer};

use super::TraceReader;

const KEEP_ENV: &str = "ZAKURA_TEST_TRACE";

static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(1);

/// A per-test Zakura trace capture guard.
pub struct TraceCapture {
    test_name: String,
    temp_dir: PathBuf,
    persist_dir: PathBuf,
    guards: Vec<JsonlTraceGuard>,
    #[cfg(test)]
    keep_override: Option<bool>,
    flushed: bool,
    finished: bool,
}

impl TraceCapture {
    /// Create a trace capture rooted in a unique temporary directory.
    pub fn for_test(test_name: impl Into<String>) -> io::Result<Self> {
        let test_name = sanitize_test_name(&test_name.into());
        let id = NEXT_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
        let base_dir = PathBuf::from("target").join("zakura-traces");
        let temp_dir = base_dir
            .join("tmp")
            .join(format!("{test_name}-{}-{id}", std::process::id()));
        let persist_dir = base_dir.join(&test_name);

        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)?;

        Ok(Self {
            test_name,
            temp_dir,
            persist_dir,
            guards: Vec::new(),
            #[cfg(test)]
            keep_override: None,
            flushed: false,
            finished: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_keep_override(
        test_name: impl Into<String>,
        keep: bool,
    ) -> io::Result<Self> {
        let mut capture = Self::for_test(test_name)?;
        capture.keep_override = Some(keep);
        Ok(capture)
    }

    /// Return this test's temporary trace directory.
    pub fn path(&self) -> &Path {
        &self.temp_dir
    }

    /// Return the path used when this capture is persisted.
    pub fn persisted_path(&self) -> &Path {
        &self.persist_dir
    }

    /// Create a tracer that writes directly under the test trace directory.
    pub fn tracer(&mut self) -> JsonlTracer {
        self.tracer_for_dir(self.temp_dir.clone())
    }

    /// Create a per-node tracer using the same directory shape as e2e traces.
    pub fn tracer_for_node(&mut self, seed: u64) -> JsonlTracer {
        self.tracer_for_dir(self.temp_dir.join(format!("node-{seed:02}")))
    }

    /// Stop all trace writers and wait for queued rows to flush.
    pub async fn flush(&mut self) {
        if self.flushed {
            return;
        }

        for guard in std::mem::take(&mut self.guards) {
            guard.shutdown().await;
        }
        self.flushed = true;
    }

    /// Load the flushed trace directory.
    pub fn reader(&self) -> io::Result<TraceReader> {
        TraceReader::load(&self.temp_dir)
    }

    /// Flush, then persist or discard this trace according to the test result
    /// and `ZAKURA_TEST_TRACE=keep`.
    pub async fn finish(mut self) -> io::Result<Option<PathBuf>> {
        self.flush().await;
        let kept = self.keep_requested();
        let persisted = if kept {
            self.persist_sync()?;
            Some(self.persist_dir.clone())
        } else {
            let _ = fs::remove_dir_all(&self.temp_dir);
            None
        };
        self.finished = true;
        Ok(persisted)
    }

    fn tracer_for_dir(&mut self, trace_dir: PathBuf) -> JsonlTracer {
        let guard = JsonlTracer::spawn_guard_with_config(trace_dir, test_trace_config());
        let tracer = guard.tracer();
        self.guards.push(guard);
        tracer
    }

    fn keep_requested(&self) -> bool {
        #[cfg(test)]
        if let Some(keep) = self.keep_override {
            return keep;
        }

        std::env::var(KEEP_ENV).is_ok_and(|value| value.eq_ignore_ascii_case("keep"))
    }

    fn should_keep_on_drop(&self) -> bool {
        std::thread::panicking() || self.keep_requested()
    }

    fn persist_sync(&self) -> io::Result<()> {
        if let Some(parent) = self.persist_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_dir_all(&self.persist_dir);
        fs::rename(&self.temp_dir, &self.persist_dir)?;
        tracing::info!(
            test = %self.test_name,
            path = %self.persist_dir.display(),
            "persisted Zakura JSONL trace"
        );
        Ok(())
    }
}

impl Drop for TraceCapture {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        if self.should_keep_on_drop() {
            let _ = self.persist_sync();
        } else {
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }
}

fn test_trace_config() -> JsonlTraceConfig {
    JsonlTraceConfig {
        channel_capacity: 65_536,
        file_flush_interval: Duration::from_millis(100),
        ..JsonlTraceConfig::default()
    }
}

fn sanitize_test_name(test_name: &str) -> String {
    let sanitized: String = test_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "unnamed".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{Arc, Mutex},
    };

    use crate::zakura::{
        trace::{
            block_sync_trace as bs_trace, commit_state_trace as cs_trace,
            header_sync_trace as hs_trace, BLOCK_SYNC_TABLE, COMMIT_STATE_TABLE, CONN_TABLE,
            HEADER_SYNC_TABLE,
        },
        ZakuraTrace, ZakuraTraceEvent,
    };

    use super::*;

    #[tokio::test]
    async fn passing_capture_discards_trace_dir() {
        let mut capture =
            TraceCapture::for_test_with_keep_override("passing_capture_discards_trace_dir", false)
                .unwrap();
        let temp = capture.path().to_path_buf();
        let persisted = capture.persisted_path().to_path_buf();
        let trace = ZakuraTrace::new(capture.tracer(), "01");

        trace.emit(CONN_TABLE, ZakuraTraceEvent::new("accepted").conn(1));

        assert!(capture.finish().await.unwrap().is_none());
        assert!(!temp.exists());
        assert!(!persisted.exists());
    }

    #[tokio::test]
    async fn keep_override_persists_trace_on_success() {
        let mut capture = TraceCapture::for_test_with_keep_override(
            "keep_override_persists_trace_on_success",
            true,
        )
        .unwrap();
        let persisted = capture.persisted_path().to_path_buf();
        let trace = ZakuraTrace::new(capture.tracer_for_node(1), "01");

        trace.emit(CONN_TABLE, ZakuraTraceEvent::new("accepted").conn(1));
        capture.flush().await;
        assert_eq!(
            capture
                .reader()
                .unwrap()
                .node("01")
                .table("conn")
                .count("accepted"),
            1
        );

        assert_eq!(capture.finish().await.unwrap(), Some(persisted.clone()));
        assert!(persisted.join("node-01").join("conn.jsonl").exists());
        let _ = fs::remove_dir_all(persisted);
    }

    #[tokio::test]
    async fn per_node_tracers_write_separate_dirs_and_node_fields() {
        let mut capture = TraceCapture::for_test_with_keep_override(
            "per_node_tracers_write_separate_dirs_and_node_fields",
            false,
        )
        .unwrap();
        let left = ZakuraTrace::new(capture.tracer_for_node(1), "01");
        let right = ZakuraTrace::new(capture.tracer_for_node(2), "02");

        left.emit(CONN_TABLE, ZakuraTraceEvent::new("accepted").conn(1));
        right.emit(CONN_TABLE, ZakuraTraceEvent::new("accepted").conn(1));
        capture.flush().await;

        let reader = capture.reader().unwrap();
        assert_eq!(reader.node("01").table("conn").count("accepted"), 1);
        assert_eq!(reader.node("02").table("conn").count("accepted"), 1);
        assert!(capture.path().join("node-01").join("conn.jsonl").exists());
        assert!(capture.path().join("node-02").join("conn.jsonl").exists());

        assert!(capture.finish().await.unwrap().is_none());
    }

    #[test]
    fn panicking_capture_persists_trace_dir() {
        let persisted_path = Arc::new(Mutex::new(None));
        let shared_path = Arc::clone(&persisted_path);

        let panic_result = catch_unwind(AssertUnwindSafe(move || {
            let capture = TraceCapture::for_test_with_keep_override(
                "panicking_capture_persists_trace_dir",
                false,
            )
            .unwrap();
            *shared_path.lock().unwrap() = Some(capture.persisted_path().to_path_buf());
            fs::write(
                capture.path().join("conn.jsonl"),
                r#"{"node":"01","event":"accepted","conn":1}"#.to_string() + "\n",
            )
            .unwrap();

            panic!("exercise TraceCapture drop persistence");
        }));

        assert!(panic_result.is_err());

        let persisted = persisted_path
            .lock()
            .unwrap()
            .clone()
            .expect("test stored persisted path before panicking");
        assert!(persisted.join("conn.jsonl").exists());

        let reader = TraceReader::load(&persisted).unwrap();
        assert_eq!(reader.node("01").table("conn").count("accepted"), 1);

        let _ = fs::remove_dir_all(persisted);
    }

    #[tokio::test]
    async fn zakura_sync_trace_tables_write_separate_files() {
        let mut capture = TraceCapture::for_test_with_keep_override(
            "zakura_sync_trace_tables_write_separate_files",
            false,
        )
        .unwrap();
        let trace = ZakuraTrace::new(capture.tracer(), "01");

        trace.emit_with(BLOCK_SYNC_TABLE, |row| {
            row.insert(
                bs_trace::EVENT.to_string(),
                serde_json::Value::String("block_test".to_string()),
            );
        });
        trace.emit_with(HEADER_SYNC_TABLE, |row| {
            row.insert(
                hs_trace::EVENT.to_string(),
                serde_json::Value::String("header_test".to_string()),
            );
        });
        trace.emit_with(COMMIT_STATE_TABLE, |row| {
            row.insert(
                cs_trace::EVENT.to_string(),
                serde_json::Value::String("commit_test".to_string()),
            );
        });

        capture.flush().await;

        assert!(capture.path().join(BLOCK_SYNC_TABLE.file_name()).exists());
        assert!(capture.path().join(HEADER_SYNC_TABLE.file_name()).exists());
        assert!(capture.path().join(COMMIT_STATE_TABLE.file_name()).exists());

        let reader = capture.reader().unwrap();
        assert_eq!(
            reader.table(BLOCK_SYNC_TABLE.table()).count("block_test"),
            1
        );
        assert_eq!(
            reader.table(HEADER_SYNC_TABLE.table()).count("header_test"),
            1
        );
        assert_eq!(
            reader
                .table(COMMIT_STATE_TABLE.table())
                .count("commit_test"),
            1
        );
        assert_eq!(
            reader.table(BLOCK_SYNC_TABLE.table()).count("commit_test"),
            0
        );
        assert_eq!(
            reader.table(HEADER_SYNC_TABLE.table()).count("commit_test"),
            0
        );

        let _ = capture.finish().await.unwrap();
    }
}
