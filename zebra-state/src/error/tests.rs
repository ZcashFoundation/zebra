//! State error classification regressions.

use super::*;
use zebra_chain::{block::Height, parameters::subsidy::SubsidyError};

#[test]
fn commit_block_error_misbehavior_scores() {
    let context_error = CommitBlockError::ValidateContextError(Box::new(
        ValidateContextError::NonSequentialBlock {
            candidate_height: Height(5),
            parent_height: Height(3),
        },
    ));
    assert_eq!(context_error.misbehavior_score(), 0);
    let duplicate = CommitBlockError::Duplicate {
        hash_or_height: None,
        location: KnownBlock::BestChain,
    };
    assert_eq!(duplicate.misbehavior_score(), 0);
    assert_eq!(CommitBlockError::WriteTaskExited.misbehavior_score(), 0);

    let auth_commitment_error = CommitBlockError::ValidateContextError(Box::new(
        ValidateContextError::InvalidBlockCommitment(
            block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                expected: [1; 32],
                actual: [2; 32],
            },
        ),
    ));
    assert_eq!(auth_commitment_error.misbehavior_score(), 100);
}

#[test]
fn contextual_subsidy_errors_score_misbehavior() {
    let direct = CommitSemanticallyVerifiedError::from(ValidateContextError::Subsidy(
        SubsidyError::InvalidMinerFees,
    ));
    assert_eq!(direct.inner().misbehavior_score(), 100);
    let contextual = CommitSemanticallyVerifiedError::from(
        ValidateContextError::CalculateBlockChainValueChange {
            value_balance_error: ValueBalanceError::Subsidy(SubsidyError::InvalidMinerFees),
            height: Height(1_000),
            block_hash: block::Hash([1; 32]),
            transaction_count: 1,
            spent_utxo_count: 0,
        },
    );
    assert_eq!(contextual.inner().misbehavior_score(), 100);
    let unrelated = CommitSemanticallyVerifiedError::from(
        ValidateContextError::CalculateBlockChainValueChange {
            value_balance_error: ValueBalanceError::Unparsable,
            height: Height(1_000),
            block_hash: block::Hash([1; 32]),
            transaction_count: 1,
            spent_utxo_count: 0,
        },
    );
    assert_eq!(unrelated.inner().misbehavior_score(), 0);
}

/// Peers serving descendants must not inherit the rejected ancestor's penalty.
#[test]
fn descendant_errors_are_not_scored() {
    let auth_commitment_error = ValidateContextError::InvalidBlockCommitment(
        block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
            expected: [1; 32],
            actual: [2; 32],
        },
    );
    let rejected_hash = block::Hash([1; 32]);
    let child_error = auth_commitment_error.for_descendant(rejected_hash);
    assert_eq!(child_error.misbehavior_score(), 0);
    assert!(!child_error.is_auth_commitment_mismatch());
    assert!(child_error.is_descendant_of_auth_commitment_mismatch());
    assert_eq!(
        child_error.for_descendant(block::Hash([2; 32])),
        child_error,
    );
    let commit_error = CommitBlockError::ValidateContextError(Box::new(child_error));
    assert_eq!(commit_error.misbehavior_score(), 0);
    assert!(!commit_error.is_auth_commitment_mismatch());
    assert!(commit_error.is_descendant_of_auth_commitment_mismatch());

    for ancestor_error in [
        ValidateContextError::NonSequentialBlock {
            candidate_height: Height(5),
            parent_height: Height(3),
        },
        ValidateContextError::Subsidy(SubsidyError::InvalidMinerFees),
        ValidateContextError::CalculateBlockChainValueChange {
            value_balance_error: ValueBalanceError::Subsidy(SubsidyError::InvalidMinerFees),
            height: Height(1_000),
            block_hash: rejected_hash,
            transaction_count: 1,
            spent_utxo_count: 0,
        },
    ] {
        let child_error = ancestor_error.for_descendant(rejected_hash);
        assert_eq!(child_error.misbehavior_score(), 0);
        assert!(!child_error.is_descendant_of_auth_commitment_mismatch());
        assert_eq!(
            CommitBlockError::ValidateContextError(Box::new(child_error)).misbehavior_score(),
            0,
        );
    }
}
