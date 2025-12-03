//! Error types for Zebra's state.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use derive_new::new;
use thiserror::Error;

use zebra_chain::{
    amount::{self, NegativeAllowed, NonNegative},
    block,
    history_tree::HistoryTreeError,
    orchard, sapling, sprout, transaction, transparent,
    value_balance::{ValueBalance, ValueBalanceError},
    work::difficulty::CompactDifficulty,
};

use crate::{constants::MIN_TRANSPARENT_COINBASE_MATURITY, HashOrHeight, KnownBlock};

/// A wrapper for type erased errors that is itself clonable and implements the
/// Error trait
#[derive(Debug, Error, Clone)]
#[error(transparent)]
pub struct CloneError {
    source: Arc<dyn std::error::Error + Send + Sync + 'static>,
}

impl From<CommitSemanticallyVerifiedError> for CloneError {
    fn from(source: CommitSemanticallyVerifiedError) -> Self {
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

/// An error describing why a block could not be queued to be committed to the state.
#[derive(Debug, Error, Clone, PartialEq, Eq, new)]
pub enum CommitBlockError {
    #[error("block hash is a duplicate: already in {location}")]
    /// The block is a duplicate: it is already queued or committed in the state.
    Duplicate {
        /// Hash or height of the duplicated block.
        hash_or_height: Option<HashOrHeight>,
        /// Location in the state where the block can be found.
        location: KnownBlock,
    },

    /// Contextual validation failed.
    #[error("could not contextually validate semantically verified block")]
    ValidateContextError(#[from] ValidateContextError),

    /// The write task exited (likely during shutdown).
    #[error("block commit task exited. Is Zebra shutting down?")]
    #[non_exhaustive]
    WriteTaskExited,
}

impl CommitBlockError {
    /// Returns `true` if this is definitely a duplicate commit request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        matches!(self, CommitBlockError::Duplicate { .. })
    }
}

/// An error describing why a `CommitSemanticallyVerified` request failed.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("could not commit semantically-verified block")]
pub struct CommitSemanticallyVerifiedError(#[from] CommitBlockError);

impl From<ValidateContextError> for CommitSemanticallyVerifiedError {
    fn from(value: ValidateContextError) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Error)]
pub enum LayeredStateError<E: std::error::Error + std::fmt::Display> {
    #[error("{0}")]
    State(E),
    #[error("{0}")]
    Layer(BoxError),
}

impl<E: std::error::Error + 'static> From<BoxError> for LayeredStateError<E> {
    fn from(err: BoxError) -> Self {
        match err.downcast::<E>() {
            Ok(state_err) => Self::State(*state_err),
            Err(layer_error) => Self::Layer(layer_error),
        }
    }
}

/// An error describing why a `CommitCheckpointVerifiedBlock` request failed.
#[derive(Debug, Error, Clone)]
#[error("could not commit checkpoint-verified block")]
pub struct CommitCheckpointVerifiedError(#[from] CommitBlockError);

impl From<ValidateContextError> for CommitCheckpointVerifiedError {
    fn from(value: ValidateContextError) -> Self {
        Self(value.into())
    }
}

/// An error describing why a `InvalidateBlock` request failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InvalidateError {
    /// The state is currently checkpointing blocks and cannot accept invalidation requests.
    #[error("cannot invalidate blocks while still committing checkpointed blocks")]
    ProcessingCheckpointedBlocks,

    /// Sending the invalidate request to the block write task failed.
    #[error("failed to send invalidate block request to block write task")]
    SendInvalidateRequestFailed,

    /// The invalidate request was dropped before processing.
    #[error("invalidate block request was unexpectedly dropped")]
    InvalidateRequestDropped,

    /// The block hash was not found in any non-finalized chain.
    #[error("block hash {0} not found in any non-finalized chain")]
    BlockNotFound(block::Hash),
}

/// An error describing why a `ReconsiderBlock` request failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReconsiderError {
    /// The block is not found in the list of invalidated blocks.
    #[error("Block with hash {0} was not previously invalidated")]
    MissingInvalidatedBlock(block::Hash),

    /// The block's parent is missing from the non-finalized state.
    #[error("Parent chain not found for block {0}")]
    ParentChainNotFound(block::Hash),

    /// There were no invalidated blocks when at least one was expected.
    #[error("Invalidated blocks list is empty when it should contain at least one block")]
    InvalidatedBlocksEmpty,

    /// The state is currently checkpointing blocks and cannot accept reconsider requests.
    #[error("cannot reconsider blocks while still committing checkpointed blocks")]
    CheckpointCommitInProgress,

    /// Sending the reconsider request to the block write task failed.
    #[error("failed to send reconsider block request to block write task")]
    ReconsiderSendFailed,

    /// The reconsider request was dropped before processing.
    #[error("reconsider block request was unexpectedly dropped")]
    ReconsiderResponseDropped,
}

/// An error describing why a block failed contextual validation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ValidateContextError {
    #[error("block hash {block_hash} was previously invalidated")]
    #[non_exhaustive]
    BlockPreviouslyInvalidated { block_hash: block::Hash },

    #[error("block parent not found in any chain, or not enough blocks in chain")]
    #[non_exhaustive]
    NotReadyToBeCommitted,

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

    #[error("transparent double-spend: {outpoint:?} is spent twice in {location:?}")]
    #[non_exhaustive]
    DuplicateTransparentSpend {
        outpoint: transparent::OutPoint,
        location: &'static str,
    },

    #[error("missing transparent output: possible double-spend of {outpoint:?} in {location:?}")]
    #[non_exhaustive]
    MissingTransparentOutput {
        outpoint: transparent::OutPoint,
        location: &'static str,
    },

    #[error("out-of-order transparent spend: {outpoint:?} is created by a later transaction in the same block")]
    #[non_exhaustive]
    EarlyTransparentSpend { outpoint: transparent::OutPoint },

    #[error(
        "unshielded transparent coinbase spend: {outpoint:?} \
         must be spent in a transaction which only has shielded outputs"
    )]
    #[non_exhaustive]
    UnshieldedTransparentCoinbaseSpend { outpoint: transparent::OutPoint },

    #[error(
        "immature transparent coinbase spend: \
        attempt to spend {outpoint:?} at {spend_height:?}, \
        but spends are invalid before {min_spend_height:?}, \
        which is {MIN_TRANSPARENT_COINBASE_MATURITY:?} blocks \
        after it was created at {created_height:?}"
    )]
    #[non_exhaustive]
    ImmatureTransparentCoinbaseSpend {
        outpoint: transparent::OutPoint,
        spend_height: block::Height,
        min_spend_height: block::Height,
        created_height: block::Height,
    },

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

    #[error(
        "the remaining value in the transparent transaction value pool MUST be nonnegative:\n\
         {amount_error:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    NegativeRemainingTransactionValue {
        amount_error: amount::Error,
        height: block::Height,
        tx_index_in_block: usize,
        transaction_hash: transaction::Hash,
    },

    #[error(
        "error calculating the remaining value in the transaction value pool:\n\
         {amount_error:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    CalculateRemainingTransactionValue {
        amount_error: amount::Error,
        height: block::Height,
        tx_index_in_block: usize,
        transaction_hash: transaction::Hash,
    },

    #[error(
        "error calculating value balances for the remaining value in the transaction value pool:\n\
         {value_balance_error:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    CalculateTransactionValueBalances {
        value_balance_error: ValueBalanceError,
        height: block::Height,
        tx_index_in_block: usize,
        transaction_hash: transaction::Hash,
    },

    #[error(
        "error calculating the block chain value pool change:\n\
         {value_balance_error:?},\n\
         {height:?}, {block_hash:?},\n\
         transactions: {transaction_count:?}, spent UTXOs: {spent_utxo_count:?}"
    )]
    #[non_exhaustive]
    CalculateBlockChainValueChange {
        value_balance_error: ValueBalanceError,
        height: block::Height,
        block_hash: block::Hash,
        transaction_count: usize,
        spent_utxo_count: usize,
    },

    #[error(
        "error adding value balances to the chain value pool:\n\
         {value_balance_error:?},\n\
         {chain_value_pools:?},\n\
         {block_value_pool_change:?},\n\
         {height:?}"
    )]
    #[non_exhaustive]
    AddValuePool {
        value_balance_error: ValueBalanceError,
        chain_value_pools: Box<ValueBalance<NonNegative>>,
        block_value_pool_change: Box<ValueBalance<NegativeAllowed>>,
        height: Option<block::Height>,
    },

    #[error("error updating a note commitment tree: {0}")]
    NoteCommitmentTreeError(#[from] zebra_chain::parallel::tree::NoteCommitmentTreeError),

    #[error("error building the history tree: {0}")]
    HistoryTreeError(#[from] Arc<HistoryTreeError>),

    #[error("block contains an invalid commitment: {0}")]
    InvalidBlockCommitment(#[from] block::CommitmentError),

    #[error(
        "unknown Sprout anchor: {anchor:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    UnknownSproutAnchor {
        anchor: sprout::tree::Root,
        height: Option<block::Height>,
        tx_index_in_block: Option<usize>,
        transaction_hash: transaction::Hash,
    },

    #[error(
        "unknown Sapling anchor: {anchor:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    UnknownSaplingAnchor {
        anchor: sapling::tree::Root,
        height: Option<block::Height>,
        tx_index_in_block: Option<usize>,
        transaction_hash: transaction::Hash,
    },

    #[error(
        "unknown Orchard anchor: {anchor:?},\n\
         {height:?}, index in block: {tx_index_in_block:?}, {transaction_hash:?}"
    )]
    #[non_exhaustive]
    UnknownOrchardAnchor {
        anchor: orchard::tree::Root,
        height: Option<block::Height>,
        tx_index_in_block: Option<usize>,
        transaction_hash: transaction::Hash,
    },
}

impl From<sprout::tree::NoteCommitmentTreeError> for ValidateContextError {
    fn from(value: sprout::tree::NoteCommitmentTreeError) -> Self {
        ValidateContextError::NoteCommitmentTreeError(value.into())
    }
}

/// Trait for creating the corresponding duplicate nullifier error from a nullifier.
pub trait DuplicateNullifierError {
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
