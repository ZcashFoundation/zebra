//! Tests for the early mining work that the block verifier starts on a block that is still being
//! verified.

use std::{sync::Arc, time::Duration};

use tower::ServiceExt;

use zebra_chain::{block::merkle, transaction::UnminedTxId};
use zebra_state as zs;
use zebra_test::mock_service::MockService;

use crate::{
    block::{Request, SemanticBlockVerifier, VerifyBlockError},
    error::BlockError,
    transaction,
};

use super::proposals::{block, network, MockState, MockTransactionVerifier};

/// The verifier asks the state for early mining work before the block's transactions are
/// verified, and keeps the work until the block is committed.
#[tokio::test]
async fn mining_candidate_is_published_before_transaction_verification() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let block = block(0);
    let hash = block.hash();
    let verify = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // The state gets the request before any transaction is sent for verification.
    let (guard, mut verification) = zs::MiningCandidateGuard::new_for_tests();
    state
        .expect_request(zs::Request::MiningCandidate(block.clone()))
        .await
        .respond(zs::Response::MiningCandidate(Some(guard)));

    let transaction_request = transaction_verifier.expect_request_that(|_| true).await;
    assert!(
        verification.has_changed().is_ok(),
        "early work is kept while the transactions are verified"
    );
    transaction_request.respond(transaction::BlockResponse {
        tx_id: UnminedTxId::from(block.transactions[0].as_ref()),
        miner_fee: None,
        sigops: 0,
    });

    // The verifier keeps the guard while the state commits the block.
    let commit = state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitSemanticallyVerifiedBlock(_))
        })
        .await;
    assert!(
        verification.has_changed().is_ok(),
        "early work is kept until the block is committed"
    );
    commit.respond(zs::Response::Committed(hash));

    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(hash)
    );
    assert!(verification.changed().await.is_err());
}

/// A block whose transactions fail verification has its early work withdrawn.
#[tokio::test]
async fn mining_candidate_is_withdrawn_when_its_block_is_rejected() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let block = block(0);
    let verify = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

    state
        .expect_request(zs::Request::KnownBlock(block.hash()))
        .await
        .respond(zs::Response::KnownBlock(None));

    let (guard, mut verification) = zs::MiningCandidateGuard::new_for_tests();
    state
        .expect_request(zs::Request::MiningCandidate(block.clone()))
        .await
        .respond(zs::Response::MiningCandidate(Some(guard)));

    transaction_verifier
        .expect_request_that(|_| true)
        .await
        .respond_error("the transaction is invalid".into());

    let result = verify.await.expect("the verifier doesn't panic");
    assert!(
        matches!(result, Err(VerifyBlockError::Transaction(_))),
        "unexpected result: {result:?}"
    );

    assert!(verification.changed().await.is_err());
    state.expect_no_requests().await;
}

/// A block gets no early work if its transactions aren't the ones its header commits to, or if
/// it fails another check that doesn't need its transactions to be verified.
#[tokio::test]
async fn mining_candidate_needs_a_body_that_is_bound_to_its_header() {
    let _init_guard = zebra_test::init();

    let network = network();

    // A block whose header commits to other transactions.
    let mut changed_body = (*block(0)).clone();
    Arc::make_mut(&mut changed_body.header).merkle_root = merkle::Root([0xff; 32]);

    // A block from too far in the future.
    let mut future_block = (*block(0)).clone();
    Arc::make_mut(&mut future_block.header).time = chrono::Utc::now() + chrono::Duration::hours(3);

    for block in [changed_body, future_block] {
        let mut state: MockState = MockService::build().for_unit_tests();
        let transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
        let verifier = SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier);

        let block = Arc::new(block);
        let verify = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

        state
            .expect_request(zs::Request::KnownBlock(block.hash()))
            .await
            .respond(zs::Response::KnownBlock(None));

        // A request for early work is never answered here, so it would time out.
        let result = tokio::time::timeout(Duration::from_secs(10), verify)
            .await
            .expect("the block is rejected without a request for early work")
            .expect("the verifier doesn't panic");
        assert!(
            matches!(
                result,
                Err(VerifyBlockError::Block {
                    source: BlockError::BadMerkleRoot { .. }
                }) | Err(VerifyBlockError::Time(_))
            ),
            "unexpected result: {result:?}"
        );
        state.expect_no_requests().await;
    }
}
