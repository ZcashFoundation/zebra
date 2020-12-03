use std::sync::Arc;
use thiserror::Error;

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
pub enum ValidateContextError {
    /// block.height is lower than the current finalized height
    #[non_exhaustive]
    OrphanedBlock,

    /// block.height is not one greater than its parent block's height
    #[non_exhaustive]
    NonSequentialBlock,

    /// block.header.time is less than or equal to the median-time-past for the block
    #[non_exhaustive]
    TimeTooEarly,

    /// block.header.time is greater than the median-time-past for the block plus 90 minutes
    #[non_exhaustive]
    TimeTooLate,

    /// block.header.difficulty_threshold is not equal to the adjusted difficulty for the block
    #[non_exhaustive]
    InvalidDifficultyThreshold,
}
