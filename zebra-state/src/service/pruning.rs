//! Bounded background deletion of finalized transaction bodies.

use std::{
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{bounded, Receiver, Sender};
use zebra_chain::{block::Height, diagnostic::task::WaitForPanics};

use super::finalized_state::{DiskWriteBatch, PruningWitness, ZebraDb};
use crate::BoxError;

#[cfg(test)]
mod tests;

/// Owns the deletion thread and joins it before state shutdown.
#[derive(Debug)]
pub(super) struct PruningTask {
    cancel: Sender<()>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl PruningTask {
    /// Starts with the durable tip, without waiting for another block notification.
    pub(super) fn spawn(db: ZebraDb, retention: u32, wake: Receiver<()>) -> Arc<Self> {
        let (cancel, cancelled) = bounded(1);
        let span = tracing::Span::current();
        let task = thread::spawn(move || {
            span.in_scope(|| {
                run(db, retention, wake, cancelled).unwrap_or_else(|error| {
                    panic!("unexpected block-body pruning failure: {error}")
                });
            });
        });
        Arc::new(Self {
            cancel,
            task: Mutex::new(Some(task)),
        })
    }

    /// Propagates worker failures through the state service's background-task checks.
    pub(super) fn check_for_panics(&self) {
        let task = {
            let mut task = self
                .task
                .lock()
                .expect("pruning task handle is not poisoned");
            if task.as_ref().is_some_and(JoinHandle::is_finished) {
                task.take()
            } else {
                None
            }
        };
        if let Some(task) = task {
            task.wait_for_panics();
        }
    }

    /// Cancels between batches and waits for any in-flight batch to finish.
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

/// Commits at most one bounded batch, returning whether another batch might be needed.
fn prune_batch(
    db: &ZebraDb,
    retention: u32,
    witness: &mut PruningWitness,
) -> Result<bool, BoxError> {
    let history = db.history_capabilities()?;
    metrics::gauge!("state.pruning.retained.height")
        .set(f64::from(history.lowest_retained_height.0));
    if !db.ready_for_pruning() {
        return Ok(false);
    }
    let Some(tip) = db.finalized_tip_height() else {
        return Ok(false);
    };
    let target = retention_floor(tip, retention);
    let previous = history.lowest_retained_height.max(Height(1));
    if target <= previous {
        return Ok(false);
    }

    let started = Instant::now();
    let mut batch = DiskWriteBatch::new();
    let Some(end) = batch.prepare_history_pruning(db, target, witness)? else {
        return Ok(false);
    };
    db.write_batch(batch)?;
    metrics::gauge!("state.pruning.retained.height").set(f64::from(end.0));
    metrics::counter!("state.pruning.deleted.height.count")
        .increment(u64::from(end.0 - previous.0));
    metrics::histogram!("state.pruning.batch.duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(true)
}

fn run(
    db: ZebraDb,
    retention: u32,
    wake: Receiver<()>,
    cancelled: Receiver<()>,
) -> Result<(), BoxError> {
    let mut witness = PruningWitness::default();
    loop {
        if cancelled.try_recv().is_ok() {
            return Ok(());
        }
        let ready = db.ready_for_pruning();
        if prune_batch(&db, retention, &mut witness)? {
            thread::yield_now();
            continue;
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
