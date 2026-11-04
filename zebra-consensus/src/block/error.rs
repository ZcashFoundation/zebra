//! Errors returned by the semantic block verifier.
//!
//! The errors for individual consensus rules live in `crate::error`.

use thiserror::Error;

use zebra_chain::{block, parameters::subsidy::SubsidyError, work::equihash};
use zebra_state as zs;

use crate::{error::*, BoxError};

/// Block verification errors.
// TODO: dedupe with crate::error::BlockError
#[non_exhaustive]
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum VerifyBlockError {
    #[error("unable to verify depth for block {hash} from chain state during block verification")]
    Depth { source: BoxError, hash: block::Hash },

    #[error(transparent)]
    Block {
        #[from]
        source: BlockError,
    },

    #[error(transparent)]
    Equihash {
        #[from]
        source: equihash::Error,
    },

    #[error(transparent)]
    Time(zebra_chain::block::BlockTimeError),

    #[error("block exceeds the maximum serialized size")]
    OversizedBlock,

    /// An authenticated early-relay body failed later transaction or contextual verification.
    #[error("authenticated early-relay block failed validation: {0}")]
    EarlyRelay(#[source] Box<VerifyBlockError>),

    /// Header/body authentication failed before the early-relay scoring exemption.
    #[error("early-relay authentication failed: {0}")]
    EarlyRelayAdmission(#[source] zs::ValidateContextError),

    /// A negotiated body's later failure could not be attributed without current parent context.
    #[error("early-relay authentication context is unavailable: {0}")]
    EarlyRelayUnavailable(#[source] Box<VerifyBlockError>),

    /// This authenticated hash recently failed deterministic transaction validation.
    #[error("authenticated early-relay block was already rejected")]
    EarlyRelayRejected,

    /// Error when attempting to commit a block after semantic verification.
    #[error("unable to commit block after semantic verification: {0}")]
    Commit(#[from] zs::CommitBlockError),

    #[error("unable to validate block proposal: failed semantic verification (proof of work is not checked for proposals): {0}")]
    // TODO: make this into a concrete type (see #5732)
    ValidateProposal(#[source] BoxError),

    #[error("invalid transaction: {0}")]
    Transaction(#[from] TransactionError),

    #[error("invalid block subsidy: {0}")]
    Subsidy(#[from] SubsidyError),

    /// Errors originating from the state service, which may arise from general failures in interacting with the state.
    /// This is for errors that are not specifically related to block depth or commit failures.
    #[error("state service error for block {hash}: {source}")]
    StateService { source: BoxError, hash: block::Hash },
}

impl VerifyBlockError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        match self {
            VerifyBlockError::EarlyRelay(source) => source.is_duplicate_request(),
            VerifyBlockError::Block { source, .. } => source.is_duplicate_request(),
            VerifyBlockError::Commit(commit_err) => commit_err.is_duplicate_request(),
            _ => false,
        }
    }

    /// Returns `true` if the block's authorizing data doesn't match the commitment in
    /// its header.
    ///
    /// See [`zs::ValidateContextError::is_auth_commitment_mismatch()`].
    pub fn is_auth_commitment_mismatch(&self) -> bool {
        match self {
            VerifyBlockError::EarlyRelay(source) => source.is_auth_commitment_mismatch(),
            VerifyBlockError::EarlyRelayAdmission(source) => source.is_auth_commitment_mismatch(),
            VerifyBlockError::Commit(commit_err) => commit_err.is_auth_commitment_mismatch(),
            _ => false,
        }
    }

    /// Returns `true` if the block was rejected only because an ancestor's authorizing
    /// data didn't match the commitment in its header.
    ///
    /// See [`zs::ValidateContextError::is_descendant_of_auth_commitment_mismatch()`].
    pub fn is_descendant_of_auth_commitment_mismatch(&self) -> bool {
        match self {
            VerifyBlockError::EarlyRelay(source) => {
                source.is_descendant_of_auth_commitment_mismatch()
            }
            VerifyBlockError::Commit(commit_err) => {
                commit_err.is_descendant_of_auth_commitment_mismatch()
            }
            _ => false,
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        use VerifyBlockError::*;
        match self {
            Block { source } => source.misbehavior_score(),
            Equihash { .. } | Subsidy(_) => 100,
            Transaction(err) => err.mempool_misbehavior_score(),
            Commit(err) => err.misbehavior_score(),
            EarlyRelayAdmission(error) => match error {
                zs::ValidateContextError::TimeTooEarly { .. }
                | zs::ValidateContextError::TimeTooLate { .. }
                | zs::ValidateContextError::BlockPreviouslyInvalidated { .. } => 0,
                _ => 100,
            },
            OversizedBlock => 100,
            _other => 0,
        }
    }

    /// Includes content-derived totals and subsidy checks, but not unavailable state or time rules.
    pub(super) fn is_deterministic_rejection(&self) -> bool {
        match self {
            Self::Transaction(error)
            | Self::Block {
                source: BlockError::Transaction(error),
            } => error.is_deterministic_for_block(),
            Self::Block {
                source:
                    BlockError::TooManyTransparentSignatureOperations { .. }
                    | BlockError::SummingMinerFees { .. },
            } => true,
            Self::Commit(zs::CommitBlockError::ValidateContextError(error)) => matches!(
                error.as_ref(),
                zs::ValidateContextError::InvalidSubsidy { .. }
                    | zs::ValidateContextError::DuplicateTransparentSpend { .. }
                    | zs::ValidateContextError::MissingTransparentOutput { .. }
                    | zs::ValidateContextError::EarlyTransparentSpend { .. }
                    | zs::ValidateContextError::UnshieldedTransparentCoinbaseSpend { .. }
                    | zs::ValidateContextError::ImmatureTransparentCoinbaseSpend { .. }
                    | zs::ValidateContextError::DuplicateSproutNullifier { .. }
                    | zs::ValidateContextError::DuplicateSaplingNullifier { .. }
                    | zs::ValidateContextError::DuplicateOrchardNullifier { .. }
                    | zs::ValidateContextError::DuplicateIronwoodNullifier { .. }
                    | zs::ValidateContextError::UnknownSproutAnchor { .. }
                    | zs::ValidateContextError::UnknownSaplingAnchor { .. }
                    | zs::ValidateContextError::UnknownOrchardAnchor { .. }
                    | zs::ValidateContextError::UnknownIronwoodAnchor { .. }
            ),
            _ => false,
        }
    }

    /// Only authenticated bodies receive the negotiated post-admission scoring exemption.
    pub(super) fn for_early_relay(self, authenticated: bool) -> Self {
        if authenticated && self.misbehavior_score() != 0 && !self.is_auth_commitment_mismatch() {
            Self::EarlyRelay(Box::new(self))
        } else {
            self
        }
    }
}

/// Converts a semantic, contextual, or mined state commit error into a [`VerifyBlockError`].
///
/// The state boxes commit errors as [`zs::CommitSemanticallyVerifiedError`], a
/// newtype around [`zs::CommitBlockError`], so the wrapper must be unwrapped
/// here for `is_duplicate_request()` and `misbehavior_score()` to classify
/// duplicate blocks as benign.
pub(super) fn map_commit_error(source: BoxError, hash: block::Hash) -> VerifyBlockError {
    if let Some(commit_err) = source
        .downcast_ref::<zs::CommitSemanticallyVerifiedError>()
        .map(zs::CommitSemanticallyVerifiedError::inner)
        .or_else(|| source.downcast_ref::<zs::CommitBlockError>())
    {
        return VerifyBlockError::Commit(commit_err.clone());
    }

    VerifyBlockError::StateService { source, hash }
}
