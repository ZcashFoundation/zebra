//! Unit tests for the full-validation [`SemanticCommit`] stage.

use std::sync::Arc;

use tower::ServiceExt;

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
use zebra_consensus::VerifyBlockError;
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::SemanticCommit;
use crate::{components::ibd::convert::VerifyAndCommitError, BoxError};

/// A duplicate-request commit error means the block is already verified and
/// committed, so the stage reports success: routing it through the engine's
/// commit-reset path would refetch a block the state already has.
#[tokio::test]
async fn duplicate_request_commit_errors_map_to_success() {
    let _init_guard = zebra_test::init();

    let mut verifier: MockService<
        zebra_consensus::Request,
        zebra_chain::block::Hash,
        PanicAssertion,
    > = MockService::build().for_unit_tests();
    let commit = SemanticCommit::new(verifier.clone());

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded test vector deserializes");
    let hash = block.hash();

    let response = tokio::spawn(commit.oneshot(super::IbdBlock {
        height: zebra_chain::block::Height(0),
        expected: hash,
        prev_expected: zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH,
        block: block.into(),
        source: None,
        supplied_trees: None,
    }));

    let duplicate_error = VerifyBlockError::Commit(zs::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zs::KnownBlock::BestChain,
    });
    assert!(duplicate_error.is_duplicate_request());

    verifier
        .expect_request_that(|request| matches!(request, zebra_consensus::Request::Commit(_)))
        .await
        .respond(Err::<zebra_chain::block::Hash, _>(BoxError::from(
            duplicate_error,
        )));

    let committed_hash = response
        .await
        .expect("stage task does not panic")
        .expect("a duplicate commit maps to success");
    assert_eq!(committed_hash, hash);
}

/// Any other commit error still fails the stage, so the engine's discard and
/// refetch (and eventually restart) handling applies.
#[tokio::test]
async fn other_commit_errors_still_fail() {
    let _init_guard = zebra_test::init();

    let mut verifier: MockService<
        zebra_consensus::Request,
        zebra_chain::block::Hash,
        PanicAssertion,
    > = MockService::build().for_unit_tests();
    let commit = SemanticCommit::new(verifier.clone());

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded test vector deserializes");
    let hash = block.hash();

    let response = tokio::spawn(commit.oneshot(super::IbdBlock {
        height: zebra_chain::block::Height(0),
        expected: hash,
        prev_expected: zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH,
        block: block.into(),
        source: None,
        supplied_trees: None,
    }));

    verifier
        .expect_request_that(|request| matches!(request, zebra_consensus::Request::Commit(_)))
        .await
        .respond(Err::<zebra_chain::block::Hash, _>(BoxError::from(
            "synthetic contextual validation failure",
        )));

    let result = response.await.expect("stage task does not panic");
    assert!(
        matches!(result, Err(VerifyAndCommitError::Commit { .. })),
        "a non-duplicate commit error fails the stage: {result:?}",
    );
}
