//! Tests for committing blocks with note commitment tree updates that were computed ahead of them.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::watch;
use tower::ServiceExt;

use zebra_chain::{block::Block, orchard, parallel::tree::PrecomputedTreeUpdate};

use crate::{
    service::{
        non_finalized_state::{NonFinalizedState, TreePrecomputer},
        ReadStateService,
    },
    ReadRequest, ReadResponse, ValidateContextError, WatchReceiver,
};

use super::tree_first::BeforeOrchardBlock;

/// How long a test waits for the thread that precomputes tree updates.
const PRECOMPUTE_TIMEOUT: Duration = Duration::from_secs(60);

/// Returns the update of `parent` with `note_commitments` that was precomputed in `state`, if it
/// is there.
fn precomputed_update(
    state: &NonFinalizedState,
    parent: &Arc<orchard::tree::NoteCommitmentTree>,
    note_commitments: &[orchard::tree::NoteCommitmentUpdate],
) -> Option<Arc<orchard::tree::NoteCommitmentTree>> {
    PrecomputedTreeUpdate::find(&state.precomputed_tree_updates(), parent, note_commitments)
        .map(|(tree, _subtree)| tree)
}

/// Precomputes the Orchard update that `block` makes to the best chain tip of `state`, from the
/// block's transactions, as if the mempool expected each of them in the next block. Returns the
/// updated tree.
fn precompute_orchard_update(
    state: &NonFinalizedState,
    block: &Block,
) -> Arc<orchard::tree::NoteCommitmentTree> {
    let parent = state
        .best_chain()
        .expect("the state has a chain")
        .orchard_note_commitment_tree_for_tip();
    let note_commitments: Vec<_> = block.orchard_note_commitments().collect();
    assert!(precomputed_update(state, &parent, &note_commitments).is_none());

    let orchard_transactions = block
        .transactions
        .iter()
        .filter(|transaction| transaction.has_orchard_shielded_data())
        .count();
    assert!(
        (1..=3).contains(&orchard_transactions),
        "the block has Orchard transactions, and all of them can have updates together",
    );

    let (_non_finalized_state_sender, non_finalized_state_receiver) = watch::channel(state.clone());
    let non_finalized_state_receiver = WatchReceiver::new(non_finalized_state_receiver);

    TreePrecomputer::default().transactions_expected(
        non_finalized_state_receiver,
        block
            .transactions
            .iter()
            .map(|transaction| (transaction.hash(), transaction.clone())),
    );

    let started = Instant::now();
    loop {
        if let Some(precomputed) = precomputed_update(state, &parent, &note_commitments) {
            return precomputed;
        }
        assert!(
            started.elapsed() < PRECOMPUTE_TIMEOUT,
            "the update for the block's transactions is precomputed",
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A block is committed with the Orchard tree that was computed ahead of it, for its
/// transactions, and the chain is the same as when the block's tree is computed for it.
#[test]
fn block_is_committed_with_its_precomputed_tree_update() {
    let _init_guard = zebra_test::init();

    let BeforeOrchardBlock {
        mut state,
        finalized_state,
        block,
    } = BeforeOrchardBlock::new();
    finalized_state.populate_with_anchors(&block.block);

    let mut expected = state.clone();
    expected
        .commit_block(block.clone(), &finalized_state)
        .expect("the block is valid");

    let precomputed = precompute_orchard_update(&state, &block.block);

    state
        .commit_block(block, &finalized_state)
        .expect("the block is valid");

    let chain = state.best_chain().expect("the state has a chain");
    assert!(
        Arc::ptr_eq(&chain.orchard_note_commitment_tree_for_tip(), &precomputed),
        "the block is committed with the precomputed tree",
    );
    assert!(state.eq_internal_state(&expected));
}

/// A block with a precomputed tree update is rejected for the same reason as without that update,
/// and it leaves the state unchanged.
#[test]
fn invalid_block_with_a_precomputed_tree_update_is_rejected() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    precompute_orchard_update(&test.state, &test.block.block);
    test.reveal_orchard_nullifier();
    let result = test.commit(true);
    assert!(
        matches!(
            result,
            Err(ValidateContextError::DuplicateOrchardNullifier { .. })
        ),
        "{result:?}",
    );

    let mut test = BeforeOrchardBlock::with_history();
    test.corrupt_block_commitment();
    precompute_orchard_update(&test.state, &test.block.block);
    let result = test.commit(true);
    assert!(
        matches!(
            result,
            Err(ValidateContextError::InvalidBlockCommitment(..))
        ),
        "{result:?}",
    );
}

/// The state starts the tree updates of the transactions that the mempool expects in the next
/// block when it is sent them, and checking a transaction for the mempool doesn't.
#[tokio::test(flavor = "multi_thread")]
async fn expected_transactions_have_their_tree_updates_precomputed() {
    let _init_guard = zebra_test::init();

    let BeforeOrchardBlock {
        state,
        finalized_state,
        block,
    } = BeforeOrchardBlock::new();
    finalized_state.populate_with_anchors(&block.block);

    let (_non_finalized_state_sender, non_finalized_state_receiver) = watch::channel(state.clone());
    let read_state = ReadStateService::new(
        &finalized_state,
        None,
        WatchReceiver::new(non_finalized_state_receiver),
    );

    let transaction = block
        .block
        .transactions
        .iter()
        .find(|transaction| transaction.has_orchard_shielded_data())
        .expect("the block has an Orchard transaction")
        .clone();
    let parent = state
        .best_chain()
        .expect("the state has a chain")
        .orchard_note_commitment_tree_for_tip();
    let note_commitments = PrecomputedTreeUpdate::orchard_note_commitments(&transaction);
    assert!(precomputed_update(&state, &parent, &note_commitments).is_none());

    // The transaction verifier's check comes before the rest of a transaction's verification.
    let response = read_state
        .clone()
        .oneshot(ReadRequest::CheckBestChainTipNullifiersAndAnchors(
            transaction.clone().into(),
        ))
        .await
        .expect("the transaction is valid on the tip");
    assert_eq!(
        response,
        ReadResponse::ValidBestChainTipNullifiersAndAnchors
    );
    assert!(
        !read_state.tree_precomputer.has_requests(),
        "a transaction that is only checked has no tree updates",
    );

    // The request is answered where it is made.
    let response = futures::FutureExt::now_or_never(
        read_state.oneshot(ReadRequest::PrecomputeTreeUpdates(vec![transaction.into()])),
    )
    .expect("the read state answers where it is called")
    .expect("the request has no errors");
    assert_eq!(response, ReadResponse::PrecomputingTreeUpdates);

    let started = Instant::now();
    while precomputed_update(&state, &parent, &note_commitments).is_none() {
        assert!(
            started.elapsed() < PRECOMPUTE_TIMEOUT,
            "the update for the transaction is precomputed",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
