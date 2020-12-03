use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;

use zebra_chain::{block, work::difficulty::CompactDifficulty};

/// A wrapper for type erased errors that is itself clonable and implements the
/// Error trait
#[derive(Debug, Error, Clone)]
#[error(transparent)]
pub struct CloneError {
    source: Arc<dyn std::error::Error + Send + Sync + 'static>,
}

impl From<CommitBlockError> for CloneError {
    fn from(source: CommitBlockError) -> Self {
        let source = Arc::new(source);
        Self { source }
    }
}

impl From<BoxError> for CloneError {
    fn from(source: BoxError) -> Self {
        let source = Arc::from(source);
        Self { source }
    }
}

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// An error describing the reason a block could not be committed to the state.
#[derive(Debug, Error)]
#[error("block is not contextually valid")]
pub struct CommitBlockError(#[from] ValidateContextError);

/// An error describing why a block failed contextual validation.
#[derive(displaydoc::Display, Debug, Error)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ValidateContextError {
    /// block height {candidate_height:?} is lower than the current finalized height {finalized_tip_height:?}
    #[non_exhaustive]
    OrphanedBlock {
        candidate_height: block::Height,
        finalized_tip_height: block::Height,
    },

    /// block height {candidate_height:?} is not one greater than its parent block's height {parent_height:?}
    #[non_exhaustive]
    NonSequentialBlock {
        candidate_height: block::Height,
        parent_height: block::Height,
    },

    /// block time {candidate_time:?} is less than or equal to the median-time-past for the block {median_time_past:?}
    #[non_exhaustive]
    TimeTooEarly {
        candidate_time: DateTime<Utc>,
        median_time_past: DateTime<Utc>,
    },

    /// block time {candidate_time:?} is greater than the median-time-past for the block plus 90 minutes {block_time_max:?}
    #[non_exhaustive]
    TimeTooLate {
        candidate_time: DateTime<Utc>,
        block_time_max: DateTime<Utc>,
    },

    /// block difficulty threshold {difficulty_threshold:?} is not equal to the expected difficulty for the block {expected_difficulty:?}
    #[non_exhaustive]
    InvalidDifficultyThreshold {
        difficulty_threshold: CompactDifficulty,
        expected_difficulty: CompactDifficulty,
    },
}
