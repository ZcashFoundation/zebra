//! Syncer state published for stall diagnosis.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use tokio::task::AbortHandle;

use zebra_chain::block;

/// Why a sync round ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartReason {
    /// The syncer downloaded every block its peers advertised.
    ExhaustedTips,
    /// Obtaining tips from peers took too long.
    ObtainTipsTimeout,
    /// Obtaining tips failed.
    ObtainTipsError,
    /// Extending tips failed.
    ExtendTipsError,
    /// A sync step took longer than the block verify timeout, which usually indicates a hang.
    SyncStepTimeout,
    /// A block download or verification failed with an error that restarts the sync.
    BlockError,
}

impl RestartReason {
    /// Returns the metric label and log field value for this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            RestartReason::ExhaustedTips => "exhausted_tips",
            RestartReason::ObtainTipsTimeout => "obtain_tips_timeout",
            RestartReason::ObtainTipsError => "obtain_tips_error",
            RestartReason::ExtendTipsError => "extend_tips_error",
            RestartReason::SyncStepTimeout => "sync_step_timeout",
            RestartReason::BlockError => "block_error",
        }
    }
}

/// A snapshot of the syncer's state, read by the progress task when sync stalls.
#[derive(Clone, Debug, Default)]
pub struct SyncDiagnostics {
    /// How the latest sync round ended.
    pub last_restart: Option<RestartReason>,

    /// The number of consecutive sync rounds that ended without the state tip advancing.
    pub restarts_without_progress: u32,

    /// The number of blocks being downloaded or verified.
    pub in_flight: usize,

    /// The blocks that might still be downloading or verifying, shared with the downloader.
    pub in_flight_blocks: InFlightBlocks,
}

impl SyncDiagnostics {
    /// Returns how long the oldest block that is still being downloaded or verified has been
    /// queued, as of `now`.
    pub fn oldest_in_flight_age(&self, now: Instant) -> Option<Duration> {
        self.in_flight_blocks
            .oldest_running_since()
            .map(|queued_at| now.saturating_duration_since(queued_at))
    }
}

/// The block download and verify tasks that might still be running, ordered by when they were
/// queued.
///
/// The downloader and the progress task share it, because a finished task stays in the downloader
/// until the syncer reads its result, which can be several sync rounds later for a node at the
/// tip, and the syncer only republishes its state during a round.
#[derive(Clone, Debug, Default)]
pub struct InFlightBlocks(Arc<Mutex<BTreeMap<(Instant, [u8; 32]), AbortHandle>>>);

impl InFlightBlocks {
    /// Adds the task for `hash`, queued at `queued_at`.
    pub fn insert(&self, queued_at: Instant, hash: block::Hash, task: AbortHandle) {
        self.tasks().insert((queued_at, hash.0), task);
    }

    /// Removes the task for `hash`, queued at `queued_at`.
    pub fn remove(&self, queued_at: Instant, hash: block::Hash) {
        self.tasks().remove(&(queued_at, hash.0));
    }

    /// Removes every task.
    pub fn clear(&self) {
        self.tasks().clear();
    }

    /// Returns when the oldest task that is still running was queued, and forgets the older ones
    /// that have finished.
    pub fn oldest_running_since(&self) -> Option<Instant> {
        let mut tasks = self.tasks();
        while let Some(oldest) = tasks.first_entry() {
            if !oldest.get().is_finished() {
                return Some(oldest.key().0);
            }
            oldest.remove();
        }

        None
    }

    fn tasks(&self) -> MutexGuard<'_, BTreeMap<(Instant, [u8; 32]), AbortHandle>> {
        self.0
            .lock()
            .expect("no code panics while holding the in-flight blocks lock")
    }
}
