//! Tests for committing solved block proposals without validating them again.

use std::{collections::HashMap, sync::Arc};

use tower::{Service, ServiceExt};

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{Block, Height},
    parameters::{
        testnet::{self, ConfiguredActivationHeights},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserialize,
    transaction::{LockTime, Transaction, UnminedTxId},
    transparent,
};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    block::{Request, SemanticBlockVerifier, VerifyBlockError},
    transaction, BoxError,
};

/// The height that the test network's halving-preserving issuance starts at. From this height,
/// the state checks the block subsidy, so the block verifier accepts any coinbase.
const BLOCK_HEIGHT: Height = Height(10);

/// Returns a network without proof of work, where blocks at [`BLOCK_HEIGHT`] can pay any subsidy.
pub(super) fn network() -> Network {
    testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(BLOCK_HEIGHT.0),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_slow_start_interval(Height::MIN)
        .with_nsm_reissuance_height(BLOCK_HEIGHT)
        .with_disable_pow(true)
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .to_network()
        .expect("network parameters are valid")
}

/// Returns a coinbase-only block at [`BLOCK_HEIGHT`], with `nonce` in its header.
pub(super) fn block(nonce: u8) -> Arc<Block> {
    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
        .expect("block is valid");

    let coinbase = Transaction::test_v4(
        vec![transparent::Input::Coinbase {
            height: BLOCK_HEIGHT,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output {
            value: Amount::<NonNegative>::try_from(1).expect("amount is valid"),
            lock_script: transparent::Script::new(&[]),
        }],
        LockTime::unlocked(),
        BLOCK_HEIGHT,
    );
    block.transactions = vec![Arc::new(coinbase)];

    let header = Arc::make_mut(&mut block.header);
    header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    header.nonce = [nonce; 32].into();

    Arc::new(block)
}

/// Returns [`block()`] with a v5 coinbase transaction that has `coinbase_data` in its input.
///
/// The ID of a v5 transaction doesn't cover its input scripts, so these blocks have the same
/// transaction ID and Merkle root for any `coinbase_data`.
fn block_with_v5_coinbase(nonce: u8, coinbase_data: u8) -> Arc<Block> {
    let mut block = (*block(nonce)).clone();

    let coinbase = Transaction::test_v5(
        NetworkUpgrade::current(&network(), BLOCK_HEIGHT),
        vec![transparent::Input::Coinbase {
            height: BLOCK_HEIGHT,
            data: vec![coinbase_data],
            sequence: u32::MAX,
        }],
        block.transactions[0].outputs(),
        LockTime::unlocked(),
        BLOCK_HEIGHT,
    );
    block.transactions = vec![Arc::new(coinbase)];

    let header = Arc::make_mut(&mut block.header);
    header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();

    Arc::new(block)
}

pub(super) type MockState = MockService<zs::Request, zs::Response, PanicAssertion, BoxError>;
pub(super) type MockTransactionVerifier =
    MockService<transaction::BlockRequest, transaction::BlockResponse, PanicAssertion, BoxError>;

/// Answers the transaction verifier's request for `block`'s coinbase transaction.
async fn verify_coinbase(transaction_verifier: &mut MockTransactionVerifier, block: &Block) {
    transaction_verifier
        .expect_request_that(|_| true)
        .await
        .respond(transaction::BlockResponse {
            tx_id: UnminedTxId::from(block.transactions[0].as_ref()),
            miner_fee: None,
            sigops: 0,
        });
}

/// Answers the requests of a successful validation of `proposal` as a block proposal.
async fn validate_proposal(
    network: &Network,
    state: &mut MockState,
    transaction_verifier: &mut MockTransactionVerifier,
    proposal: &Arc<Block>,
) {
    state
        .expect_request(zs::Request::KnownBlock(proposal.hash()))
        .await
        .respond(zs::Response::KnownBlock(None));
    verify_coinbase(transaction_verifier, proposal).await;

    let contextual = zs::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        proposal.clone().into(),
        HashMap::new(),
        DeferredPoolBalanceChange::zero(),
        network,
        Default::default(),
    )
    .expect("the block's value balance is valid");
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CheckBlockProposalValidity(_))
        })
        .await
        .respond(zs::Response::ValidBlockProposal(Box::new(contextual)));
}

/// Answers the requests of a successful verification of `block` that doesn't use a proposal.
async fn verify_block(
    state: &mut MockState,
    transaction_verifier: &mut MockTransactionVerifier,
    block: &Arc<Block>,
) {
    state
        .expect_request(zs::Request::KnownBlock(block.hash()))
        .await
        .respond(zs::Response::KnownBlock(None));
    state
        .expect_request(zs::Request::MiningCandidate(block.clone()))
        .await
        .respond(zs::Response::MiningCandidate(None));
    verify_coinbase(transaction_verifier, block).await;
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitSemanticallyVerifiedBlock(_))
        })
        .await
        .respond(zs::Response::Committed(block.hash()));
}

/// Answers the commit of a validation result for `solved`, checking that the result was rebound
/// to `solved`.
async fn commit_solved_proposal(state: &mut MockState, solved: &Arc<Block>) {
    let expected = solved.clone();
    state
        .expect_request_that(move |request| {
            let zs::Request::CommitContextuallyVerifiedBlock(contextual) = request else {
                return false;
            };
            let committed = zs::SemanticallyVerifiedBlock::from(contextual.as_ref().clone());

            committed.hash == expected.hash() && committed.block.header == expected.header
        })
        .await
        .respond(zs::Response::Committed(solved.hash()));
}

/// A block that was validated as a proposal, and then solved, is committed with the proposal's
/// validation result. Its transactions aren't verified again, and the result is only used once.
#[tokio::test]
async fn solved_proposal_is_committed_without_validating_it_again() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let proposal = block(0);
    let solved = block(1);
    assert_ne!(proposal.hash(), solved.hash());

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    // The solved proposal only needs the state to commit the proposal's validation result. That
    // is its only state request: the commit rejects a block that is already in the state.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    commit_solved_proposal(&mut state, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
    transaction_verifier.expect_no_requests().await;

    // The validation result was used, so the same block is verified in full the next time.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    verify_block(&mut state, &mut transaction_verifier, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
}

/// Proposals that only differ in their nonce are the same proposal, so the verifier keeps one
/// validation result for them. Once a block has used it, another block with that header gets the
/// known block check before its proof of work is checked, like any other block.
#[tokio::test]
async fn proposals_with_one_header_keep_one_validation_result() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    for proposal in [block(0), block(2)] {
        let verify = tokio::spawn(
            verifier
                .ready()
                .await
                .expect("the verifier is always ready")
                .call(Request::CheckProposal(proposal.clone())),
        );
        validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
        assert_eq!(
            verify.await.expect("the verifier doesn't panic").ok(),
            Some(proposal.hash())
        );
    }

    let solved = block(1);
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    commit_solved_proposal(&mut state, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );

    // A second result for this header would be committed here, without the known block check.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    verify_block(&mut state, &mut transaction_verifier, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
}

/// A block that differs from a validated proposal in more than its nonce and solution is
/// verified in full.
#[tokio::test]
async fn changed_proposal_is_verified_in_full() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let proposal = block(0);

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    // A miner that changes the block's time has changed the proposal.
    let mut changed = (*block(1)).clone();
    let header = Arc::make_mut(&mut changed.header);
    header.time += chrono::Duration::seconds(1);
    let changed = Arc::new(changed);

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(changed.clone())),
    );
    verify_block(&mut state, &mut transaction_verifier, &changed).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(changed.hash())
    );
}

/// A block with the header and transaction IDs of a validated proposal, but different
/// authorizing data, is verified in full. It doesn't use up the proposal's validation result.
#[tokio::test]
async fn proposal_with_changed_authorizing_data_is_verified_in_full() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let proposal = block_with_v5_coinbase(0, 0);

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    // This block has the proposal's header, apart from the nonce, because its coinbase
    // transaction has the same ID with a different input script.
    let changed = block_with_v5_coinbase(1, 1);
    assert_eq!(changed.header.merkle_root, proposal.header.merkle_root);
    assert_eq!(
        changed.transactions[0].hash(),
        proposal.transactions[0].hash()
    );
    assert_ne!(
        changed.transactions[0].unmined_id(),
        proposal.transactions[0].unmined_id()
    );

    // The block has the proposal's header, so the verifier only asks the state if it is new once
    // its transactions turn out to differ from the proposal's. The mocked transaction verifier
    // and state accept the block. The real ones check the changed script, and the header's
    // commitment to it.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(changed.clone())),
    );
    verify_block(&mut state, &mut transaction_verifier, &changed).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(changed.hash())
    );

    // The block that solves the proposal still gets the proposal's validation result.
    let solved = block_with_v5_coinbase(2, 0);
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    commit_solved_proposal(&mut state, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
    transaction_verifier.expect_no_requests().await;
}

/// A block that matches a validated proposal still needs a valid proof of work. A block with an
/// invalid solution is rejected before any state request, and doesn't use up the proposal.
#[tokio::test]
async fn solved_proposal_needs_valid_proof_of_work() {
    let _init_guard = zebra_test::init();

    // The Canopy activation block is the first Mainnet block that the semantic verifier accepts,
    // and its only transaction is its coinbase.
    let network = Network::Mainnet;
    let solved = Arc::new(
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
            .expect("block is valid"),
    );
    assert_eq!(solved.transactions.len(), 1);
    let with_nonce = |nonce: u8| {
        let mut block = (*solved).clone();
        Arc::make_mut(&mut block.header).nonce = [nonce; 32].into();
        Arc::new(block)
    };
    let (proposal, unsolved) = (with_nonce(0), with_nonce(1));

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    // The proposal with another nonce has no valid proof of work.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(unsolved)),
    );
    state.expect_no_requests().await;
    transaction_verifier.expect_no_requests().await;
    let result = verify.await.expect("the verifier doesn't panic");
    assert!(result.is_err(), "unexpected result: {result:?}");

    // The block with the valid solution still gets the proposal's validation result.
    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    commit_solved_proposal(&mut state, &solved).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
    transaction_verifier.expect_no_requests().await;
}

/// Returns the error that the state answers with when it already has a block.
fn duplicate_block_error() -> BoxError {
    // The state answers a commit of a validation result like the commit of any other block.
    Box::new(zs::CommitSemanticallyVerifiedError::from(
        zs::CommitBlockError::Duplicate {
            hash_or_height: None,
            location: zs::KnownBlock::BestChain,
        },
    ))
}

/// A solved proposal skips the check that the block is new, so the state's commit is what
/// rejects a block it already has. That error is still a duplicate block error, which
/// `submitblock` reports as a duplicate, and which doesn't count against a peer.
#[tokio::test]
async fn duplicate_solved_proposal_keeps_its_error() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let (proposal, solved) = (block(0), block(1));

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitContextuallyVerifiedBlock(_))
        })
        .await
        .respond_error(duplicate_block_error());

    let error = verify
        .await
        .expect("the verifier doesn't panic")
        .expect_err("the state already has the block");
    assert!(
        matches!(error, VerifyBlockError::Commit(_)),
        "unexpected error: {error:?}"
    );
    assert!(error.is_duplicate_request());
    assert_eq!(error.misbehavior_score(), 0);

    state.expect_no_requests().await;
    transaction_verifier.expect_no_requests().await;
}

/// A proposal's validation result is used once, also while its commit is still in progress.
/// Another request for the same block is verified in full, starting with the check that the
/// block is new.
#[tokio::test]
async fn solved_proposal_is_verified_in_full_while_it_is_committed() {
    let _init_guard = zebra_test::init();

    let network = network();
    let mut state: MockState = MockService::build().for_unit_tests();
    let mut transaction_verifier: MockTransactionVerifier = MockService::build().for_unit_tests();
    let mut verifier =
        SemanticBlockVerifier::new(&network, state.clone(), transaction_verifier.clone());

    let (proposal, solved) = (block(0), block(1));

    let verify = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::CheckProposal(proposal.clone())),
    );
    validate_proposal(&network, &mut state, &mut transaction_verifier, &proposal).await;
    assert_eq!(
        verify.await.expect("the verifier doesn't panic").ok(),
        Some(proposal.hash())
    );

    // The first request takes the validation result, and waits for the state to commit it.
    let first = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    let first_commit = state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitContextuallyVerifiedBlock(_))
        })
        .await;

    // The second request finds no validation result, so it asks the state if the block is new
    // before anything else. The mocked state lets it go on to its own commit.
    let second = tokio::spawn(
        verifier
            .ready()
            .await
            .expect("the verifier is always ready")
            .call(Request::Commit(solved.clone())),
    );
    state
        .expect_request(zs::Request::KnownBlock(solved.hash()))
        .await
        .respond(zs::Response::KnownBlock(None));
    state
        .expect_request(zs::Request::MiningCandidate(solved.clone()))
        .await
        .respond(zs::Response::MiningCandidate(None));
    verify_coinbase(&mut transaction_verifier, &solved).await;
    state
        .expect_request_that(|request| {
            matches!(request, zs::Request::CommitSemanticallyVerifiedBlock(_))
        })
        .await
        .respond_error(duplicate_block_error());

    let error = second
        .await
        .expect("the verifier doesn't panic")
        .expect_err("the state is committing the block");
    assert!(error.is_duplicate_request(), "unexpected error: {error:?}");

    first_commit.respond(zs::Response::Committed(solved.hash()));
    assert_eq!(
        first.await.expect("the verifier doesn't panic").ok(),
        Some(solved.hash())
    );
}
