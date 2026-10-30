//! Syncer state published for stall diagnosis.

use std::time::Instant;

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

    /// When the oldest block that is still being downloaded or verified was queued.
    pub oldest_in_flight_since: Option<Instant>,
}
