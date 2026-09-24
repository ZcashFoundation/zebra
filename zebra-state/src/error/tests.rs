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
