//! Supporting types for checkpoint-based block verification

use std::cmp::Ordering;

use zebra_chain::types::BlockHeight;

use Progress::*;
use Target::*;

/// A `CheckpointVerifier`'s current progress verifying the chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progress<HeightOrHash> {
    /// We have not verified any blocks yet.
    BeforeGenesis,
    /// We have verified up to and including this checkpoint.
    PreviousCheckpoint(HeightOrHash),
    /// We have finished verifying.
    ///
    /// The final checkpoint is not included in this variant. The verifier has
    /// finished, so the checkpoints aren't particularly useful.
    /// To get the value of the final checkpoint, use `checkpoint_list.max_height()`.
    FinalCheckpoint,
}

/// Block height progress, in chain order.
impl Ord for Progress<BlockHeight> {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }
        match (self, other) {
            (BeforeGenesis, _) => Ordering::Less,
            (_, BeforeGenesis) => Ordering::Greater,
            (FinalCheckpoint, _) => Ordering::Greater,
            (_, FinalCheckpoint) => Ordering::Less,
            (PreviousCheckpoint(self_height), PreviousCheckpoint(other_height)) => {
                self_height.cmp(other_height)
            }
        }
    }
}

/// Partial order for block height progress.
///
/// The partial order must match the total order.
impl PartialOrd for Progress<BlockHeight> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A `CheckpointVerifier`'s target checkpoint, based on the current queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target<HeightOrHash> {
    /// We need more blocks before we can choose a target checkpoint.
    WaitingForBlocks,
    /// We want to verify this checkpoint.
    ///
    /// The target checkpoint can be multiple checkpoints ahead of the previous
    /// checkpoint.
    Checkpoint(HeightOrHash),
    /// We have finished verifying, there will be no more targets.
    FinishedVerifying,
}

/// Block height target, in chain order.
///
/// `WaitingForBlocks` is incomparable with itself and `Checkpoint(_)`.
impl PartialOrd for Target<BlockHeight> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            // FinishedVerifying is the final state
            (FinishedVerifying, FinishedVerifying) => Some(Ordering::Equal),
            (FinishedVerifying, _) => Some(Ordering::Greater),
            (_, FinishedVerifying) => Some(Ordering::Less),
            // Checkpoints are comparable with each other by height
            (Checkpoint(self_height), Checkpoint(other_height)) => {
                self_height.partial_cmp(other_height)
            }
            // We can wait for blocks before or after any target checkpoint,
            // so there is no ordering between checkpoint and waiting.
            (WaitingForBlocks, Checkpoint(_)) => None,
            (Checkpoint(_), WaitingForBlocks) => None,
            // However, we consider waiting equal to itself.
            (WaitingForBlocks, WaitingForBlocks) => Some(Ordering::Equal),
        }
    }
}
