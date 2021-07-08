use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;

use zebra_chain::{
    block, orchard, sapling, sprout, transparent, work::difficulty::CompactDifficulty,
};

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
#[derive(Debug, Error, PartialEq, Eq)]
#[error("block is not contextually valid")]
pub struct CommitBlockError(#[from] ValidateContextError);

/// An error describing why a block failed contextual validation.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ValidateContextError {
    #[error("block height {candidate_height:?} is lower than the current finalized height {finalized_tip_height:?}")]
    #[non_exhaustive]
    OrphanedBlock {
        candidate_height: block::Height,
        finalized_tip_height: block::Height,
    },

    #[error("block height {candidate_height:?} is not one greater than its parent block's height {parent_height:?}")]
    #[non_exhaustive]
    NonSequentialBlock {
        candidate_height: block::Height,
        parent_height: block::Height,
    },

    #[error("block time {candidate_time:?} is less than or equal to the median-time-past for the block {median_time_past:?}")]
    #[non_exhaustive]
    TimeTooEarly {
        candidate_time: DateTime<Utc>,
        median_time_past: DateTime<Utc>,
    },

    #[error("block time {candidate_time:?} is greater than the median-time-past for the block plus 90 minutes {block_time_max:?}")]
    #[non_exhaustive]
    TimeTooLate {
        candidate_time: DateTime<Utc>,
        block_time_max: DateTime<Utc>,
    },

    #[error("block difficulty threshold {difficulty_threshold:?} is not equal to the expected difficulty for the block {expected_difficulty:?}")]
    #[non_exhaustive]
    InvalidDifficultyThreshold {
        difficulty_threshold: CompactDifficulty,
        expected_difficulty: CompactDifficulty,
    },

    #[error("transparent double-spend: {out_point:?} is spent twice in {location:?}")]
    #[non_exhaustive]
    DuplicateTransparentSpend {
        out_point: transparent::OutPoint,
        location: &'static str,
    },

    #[error("missing transparent output: possible double-spend of {out_point:?} in {location:?}")]
    #[non_exhaustive]
    MissingTransparentOutput {
        out_point: transparent::OutPoint,
        location: &'static str,
    },

    #[error("out-of-order transparent spend: {out_point:?} is created by a later transaction in the same block")]
    #[non_exhaustive]
    EarlyTransparentSpend { out_point: transparent::OutPoint },

    #[error("sprout double-spend: duplicate nullifier: {nullifier:?}, in finalized state: {in_finalized_state:?}")]
    #[non_exhaustive]
    DuplicateSproutNullifier {
        nullifier: sprout::Nullifier,
        in_finalized_state: bool,
    },

    #[error("sapling double-spend: duplicate nullifier: {nullifier:?}, in finalized state: {in_finalized_state:?}")]
    #[non_exhaustive]
    DuplicateSaplingNullifier {
        nullifier: sapling::Nullifier,
        in_finalized_state: bool,
    },

    #[error("orchard double-spend: duplicate nullifier: {nullifier:?}, in finalized state: {in_finalized_state:?}")]
    #[non_exhaustive]
    DuplicateOrchardNullifier {
        nullifier: orchard::Nullifier,
        in_finalized_state: bool,
    },
}

/// Trait for creating the corresponding duplicate nullifier error from a nullifier.
pub(crate) trait DuplicateNullifierError {
    /// Returns the corresponding duplicate nullifier error for `self`.
    fn duplicate_nullifier_error(&self, in_finalized_state: bool) -> ValidateContextError;
}

impl DuplicateNullifierError for sprout::Nullifier {
    fn duplicate_nullifier_error(&self, in_finalized_state: bool) -> ValidateContextError {
        ValidateContextError::DuplicateSproutNullifier {
            nullifier: *self,
            in_finalized_state,
        }
    }
}

impl DuplicateNullifierError for sapling::Nullifier {
    fn duplicate_nullifier_error(&self, in_finalized_state: bool) -> ValidateContextError {
        ValidateContextError::DuplicateSaplingNullifier {
            nullifier: *self,
            in_finalized_state,
        }
    }
}

impl DuplicateNullifierError for orchard::Nullifier {
    fn duplicate_nullifier_error(&self, in_finalized_state: bool) -> ValidateContextError {
        ValidateContextError::DuplicateOrchardNullifier {
            nullifier: *self,
            in_finalized_state,
        }
    }
}
