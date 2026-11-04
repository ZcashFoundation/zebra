//! Bounded background deletion of finalized transaction bodies.

use std::{
    ops::Range,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{bounded, Receiver, Sender};
use zebra_chain::{block::Height, diagnostic::task::WaitForPanics};

use super::finalized_state::{DiskWriteBatch, PruningWitness, ZebraDb, MAX_PRUNING_BATCH_HEIGHTS};
use crate::BoxError;

#[cfg(test)]
mod tests;

/// Owns the deletion thread and joins it before state shutdown.
#[derive(Debug)]
pub(super) struct PruningTask {
    cancel: Sender<()>,
    /// Sole-owned by `Drop`; shared callers wait on `finished` instead of taking this handle.
    task: Option<JoinHandle<()>>,
    finished: Arc<AtomicBool>,
    completed: Receiver<thread::Result<()>>,
}

impl PruningTask {
    /// Starts with the durable tip, without waiting for another block notification.
    pub(super) fn spawn(db: ZebraDb, retention: u32, wake: Receiver<()>) -> Arc<Self> {
        let (cancel, cancelled) = bounded(1);
        let (worker_completed, completed) = bounded(1);
        let span = tracing::Span::current();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let task = thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                span.in_scope(|| {
                    run(db, retention, wake, cancelled).unwrap_or_else(|error| {
                        panic!("unexpected block-body pruning failure: {error}")
                    });
                });
            }));
            let _ = worker_completed.send(result);
            worker_finished.store(true, Ordering::Release);
        });
        Arc::new(Self {
            cancel,
            task: Some(task),
            finished,
            completed,
        })
    }

    /// Propagates the single worker outcome to at most one shared caller.
    fn propagate_worker_panic(&self) {
        if let Ok(Err(panic)) = self.completed.try_recv() {
            resume_unwind(panic);
        }
    }

    /// Propagates worker failures through the state service's background-task checks.
    pub(super) fn check_for_panics(&self) {
        if self.finished.load(Ordering::Acquire) {
            self.propagate_worker_panic();
        }
    }

    /// Cancels between batches and compaction ranges, and waits for any in-flight batch or
    /// range to finish.
    pub(super) fn stop(&self) {
        let _ = self.cancel.try_send(());
        // Blocks until the worker sends its outcome. If an earlier call already took it, the
        // worker has dropped its sender, so this returns at once.
        let outcome = self.completed.recv();
        // The worker sends its outcome as its last step, so its thread exits right after.
        while self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            thread::yield_now();
        }
        if let Ok(Err(panic)) = outcome {
            resume_unwind(panic);
        }
    }
}

impl Drop for PruningTask {
    fn drop(&mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            task.wait_for_panics();
        }
    }
}

/// Returns the first retained non-genesis height for the durable finalized tip.
fn retention_floor(tip: Height, retention: u32) -> Height {
    let end = tip
        .0
        .checked_add(1)
        .expect("valid heights are below u32::MAX");
    Height(end.saturating_sub(retention).max(1))
}

/// Commits at most one bounded batch, returning the deleted height range.
fn prune_batch(
    db: &ZebraDb,
    retention: u32,
    witness: &mut PruningWitness,
) -> Result<Option<Range<Height>>, BoxError> {
    let history = db.history_capabilities()?;
    metrics::gauge!("state.pruning.retained.height")
        .set(f64::from(history.lowest_retained_height.0));
    let ready = db.ready_for_pruning();
    if !ready {
        return Ok(None);
    }
    let Some(tip) = db.finalized_tip_height() else {
        return Ok(None);
    };
    let target = retention_floor(tip, retention);
    let previous = history.lowest_retained_height.max(Height(1));
    if target <= previous {
        return Ok(None);
    }

    let started = Instant::now();
    let mut batch = DiskWriteBatch::new();
    let Some(end) =
        batch.prepare_history_pruning(db, target, history, ready, Some(tip), witness)?
    else {
        return Ok(None);
    };
    db.write_batch(batch)?;
    // `run` repeats a batch that reports progress at once, and only waits for a finalized block
    // after `None`. A deletion that leaves the derived floor where it was would therefore spin,
    // writing an empty range tombstone per iteration: stop and wait instead.
    let floor = db.history_capabilities()?.lowest_retained_height;
    if floor <= previous {
        tracing::warn!(
            ?previous,
            ?end,
            ?floor,
            "pruning batch did not advance the retained-body floor; waiting for the next block"
        );
        return Ok(None);
    }
    metrics::gauge!("state.pruning.retained.height").set(f64::from(end.0));
    metrics::counter!("state.pruning.deleted.height.count")
        .increment(u64::from(end.0 - previous.0));
    metrics::histogram!("state.pruning.batch.duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(Some(previous..end))
}

/// The number of heights of transaction bodies each manual compaction covers, so that
/// cancellation waits for at most one range, not for a compaction of the whole pruned history.
const COMPACTION_RANGE_HEIGHTS: u32 = 50_000;

/// Reclaims tombstoned bodies after catch-up, not after each steady-state batch.
/// More than one batch's worth of deletion distinguishes a backlog from normal finalization.
/// Earlier pruning also triggers one startup compaction, in case shutdown interrupted reclamation.
///
/// Compacts from the start of the column family, so it also covers bodies deleted before a
/// restart, in ranges of [`COMPACTION_RANGE_HEIGHTS`]. Returns `false` if `cancelled` stopped it
/// between two ranges.
fn compact_catch_up(
    db: &ZebraDb,
    deleted: Range<Height>,
    previously_pruned: bool,
    cancelled: &Receiver<()>,
) -> bool {
    if !previously_pruned && deleted.end.0 - deleted.start.0 <= MAX_PRUNING_BATCH_HEIGHTS {
        return true;
    }
    let started = Instant::now();
    tracing::info!(?deleted, "compacting pruned block bodies after catch-up");
    let mut start = Height::MIN;
    while start < deleted.end {
        if cancelled.try_recv().is_ok() {
            tracing::info!(
                ?start,
                "stopped compacting pruned block bodies for shutdown"
            );
            return false;
        }
        let end = Height(
            start
                .0
                .saturating_add(COMPACTION_RANGE_HEIGHTS)
                .min(deleted.end.0),
        );
        db.compact_pruned_bodies(start..end);
        start = end;
    }
    tracing::info!(
        elapsed_seconds = started.elapsed().as_secs_f64(),
        "finished compacting pruned block bodies"
    );
    true
}

fn run(
    db: ZebraDb,
    retention: u32,
    wake: Receiver<()>,
    cancelled: Receiver<()>,
) -> Result<(), BoxError> {
    let mut witness = PruningWitness::default();
    let retained_floor = db.history_capabilities()?.lowest_retained_height;
    let mut previously_pruned = retained_floor > Height(1);
    let mut deleted = previously_pruned.then_some(retained_floor..retained_floor);
    loop {
        if cancelled.try_recv().is_ok() {
            return Ok(());
        }
        let ready = db.ready_for_pruning();
        if let Some(batch) = prune_batch(&db, retention, &mut witness)? {
            match &mut deleted {
                Some(deleted) => deleted.end = batch.end,
                None => deleted = Some(batch),
            }
            thread::yield_now();
            continue;
        }
        if ready {
            if let Some(deleted) = deleted.take() {
                if !compact_catch_up(&db, deleted, previously_pruned, &cancelled) {
                    return Ok(());
                }
                previously_pruned = false;
            }
        }

        // Readiness can change without another block, but idle ready workers need no timer.
        if ready {
            crossbeam_channel::select! {
                recv(cancelled) -> _ => return Ok(()),
                recv(wake) -> result => if result.is_err() { return Ok(()); },
            }
        } else {
            crossbeam_channel::select! {
                recv(cancelled) -> _ => return Ok(()),
                recv(wake) -> result => if result.is_err() { return Ok(()); },
                default(Duration::from_millis(100)) => {},
            }
        }
    }
}
