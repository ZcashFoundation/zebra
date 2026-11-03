//! Bounded background deletion of finalized transaction bodies.

use std::{
    ops::Range,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
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
    task: Mutex<Option<JoinHandle<()>>>,
    finished: Arc<AtomicBool>,
}

impl PruningTask {
    /// Starts with the durable tip, without waiting for another block notification.
    pub(super) fn spawn(db: ZebraDb, retention: u32, wake: Receiver<()>) -> Arc<Self> {
        let (cancel, cancelled) = bounded(1);
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
            worker_finished.store(true, Ordering::Release);
            if let Err(panic) = result {
                resume_unwind(panic);
            }
        });
        Arc::new(Self {
            cancel,
            task: Mutex::new(Some(task)),
            finished,
        })
    }

    /// Propagates worker failures through the state service's background-task checks.
    pub(super) fn check_for_panics(&self) {
        if !self.finished.load(Ordering::Acquire) {
            return;
        }
        let task = self
            .task
            .lock()
            .expect("pruning task handle is not poisoned")
            .take();
        if let Some(task) = task {
            task.wait_for_panics();
        }
    }

    /// Cancels before each batch or compaction and waits for any in-flight work to finish.
    pub(super) fn stop(&self) {
        let _ = self.cancel.try_send(());
        let task = self
            .task
            .lock()
            .expect("pruning task handle is not poisoned")
            .take();
        if let Some(task) = task {
            task.wait_for_panics();
        }
    }
}

impl Drop for PruningTask {
    fn drop(&mut self) {
        self.stop();
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
        #[cfg(test)]
        tests::CANCEL_DURING_IDLE_BATCH.with(|cancel| {
            if let Some(cancel) = cancel.borrow_mut().take() {
                cancel.send(()).unwrap();
            }
        });
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
    metrics::gauge!("state.pruning.retained.height").set(f64::from(end.0));
    metrics::counter!("state.pruning.deleted.height.count")
        .increment(u64::from(end.0 - previous.0));
    metrics::histogram!("state.pruning.batch.duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(Some(previous..end))
}

/// Reclaims tombstoned bodies after catch-up, not after each steady-state batch.
/// More than one batch's worth of deletion distinguishes a backlog from normal finalization.
/// Earlier pruning also triggers one startup compaction, in case shutdown interrupted reclamation.
fn compact_catch_up(db: &ZebraDb, deleted: Range<Height>, previously_pruned: bool) {
    if !previously_pruned && deleted.end.0 - deleted.start.0 <= MAX_PRUNING_BATCH_HEIGHTS {
        return;
    }
    let started = Instant::now();
    tracing::info!(?deleted, "compacting pruned block bodies after catch-up");
    db.compact_pruned_bodies(deleted.end);
    tracing::info!(
        elapsed_seconds = started.elapsed().as_secs_f64(),
        "finished compacting pruned block bodies"
    );
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
                if cancelled.try_recv().is_ok() {
                    return Ok(());
                }
                compact_catch_up(&db, deleted, previously_pruned);
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
