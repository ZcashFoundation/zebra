//! The mempool tells the state which transactions to expect in the next block, so the state can
//! compute that block's note commitment tree updates before the block arrives.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::sync::mpsc;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceBuilder, ServiceExt};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTxId, VerifiedUnminedTx},
};
use zebra_consensus::{error::TransactionError, transaction as tx};
use zebra_network as zn;
use zebra_node_services::mempool::{Gossip, Request, Response};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::components::{
    mempool::{self, Mempool},
    sync::{RecentSyncLengths, SyncStatus},
};

type StateService = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type MockTxVerifier =
    MockService<tx::MempoolRequest, tx::MempoolResponse, PanicAssertion, TransactionError>;

/// Returns an enabled mempool with the cost limit `tx_cost_limit` on a state with the Mainnet
/// genesis block, with that state, the mock transaction verifier, and the transactions of each
/// [`zs::ReadRequest::PrecomputeTreeUpdates`] that the mempool sends to its read state.
async fn setup(
    tx_cost_limit: u64,
) -> (
    Mempool,
    StateService,
    MockTxVerifier,
    RecentSyncLengths,
    mpsc::UnboundedReceiver<Vec<UnminedTxId>>,
) {
    let network = Network::Mainnet;
    let (state, read_state, latest_chain_tip, mut chain_tip_change) =
        zs::init(zs::Config::ephemeral(), &network, Height::MAX, 0).await;
    let mut state = ServiceBuilder::new().buffer(10).service(state);

    let (expected_sender, expected) = mpsc::unbounded_channel();
    let read_state = tower::service_fn(move |request: zs::ReadRequest| {
        if let zs::ReadRequest::PrecomputeTreeUpdates(transactions) = &request {
            let ids = transactions.iter().map(|tx| tx.id).collect();
            expected_sender
                .send(ids)
                .expect("the test keeps the receiver");
        }
        read_state.clone().oneshot(request)
    });

    let peer_set: MockService<zn::Request, zn::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let tx_verifier: MockTxVerifier = MockService::build().for_unit_tests();
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (misbehavior_tx, _misbehavior_rx) = mpsc::channel(1);
    let (mut mempool, transaction_subscriber, _transaction_verified) = Mempool::new(
        &network,
        &mempool::Config {
            tx_cost_limit,
            ..Default::default()
        },
        Buffer::new(BoxService::new(peer_set), 1),
        state.clone(),
        tower::util::BoxCloneService::new(read_state),
        Buffer::new(BoxService::new(tx_verifier.clone()), 1),
        sync_status,
        latest_chain_tip,
        chain_tip_change.clone(),
        misbehavior_tx,
    );
    let mut changes = transaction_subscriber.subscribe();
    tokio::spawn(async move { while changes.recv().await.is_ok() {} });

    commit(
        &mut state,
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES,
    )
    .await;
    chain_tip_change
        .wait_for_tip_change()
        .await
        .expect("the state publishes the genesis block");
    mempool.enable(&mut recent_syncs).await;

    (mempool, state, tx_verifier, recent_syncs, expected)
}

/// Commits the checkpoint block in `bytes` to `state`.
async fn commit(state: &mut StateService, bytes: &[u8]) {
    let block: Arc<Block> = bytes
        .zcash_deserialize_into()
        .expect("test vector blocks are valid");
    state
        .ready()
        .await
        .expect("the state is ready")
        .call(zs::Request::CommitCheckpointVerifiedBlock(block.into()))
        .await
        .expect("test vector blocks are valid");
}

/// Returns distinct test vector transactions with Orchard actions, with fee weight ratios that
/// rise along the list, and a transaction without Orchard or Ironwood actions that pays more
/// than any of them.
fn transactions() -> (Vec<VerifiedUnminedTx>, VerifiedUnminedTx) {
    let network = Network::Mainnet;
    let mut ids = HashSet::new();
    let mut orchard: Vec<_> = network
        .unmined_transactions_in_blocks(..)
        .filter(|tx| tx.transaction.transaction.has_orchard_shielded_data())
        .filter(|tx| ids.insert(tx.transaction.id))
        .collect();
    assert!(
        orchard.len() >= 2,
        "the vectors have {} Orchard transactions",
        orchard.len(),
    );
    for (ratio, transaction) in (1u8..).zip(&mut orchard) {
        transaction.fee_weight_ratio = f32::from(ratio);
    }

    let mut other = network
        .unmined_transactions_in_blocks(..)
        .find(|tx| {
            !tx.transaction.transaction.is_coinbase()
                && !mempool::storage::changes_orchard_or_ironwood_tree(&tx.transaction)
        })
        .expect("the vectors have a transaction without Orchard actions");
    other.fee_weight_ratio = 1_000.0;

    (orchard, other)
}

/// Queues `transaction` in `mempool`, verifies it with the mock verifier, and waits until the
/// mempool has stored it. Returns false if the mempool evicted it at once.
async fn accept(
    mempool: &mut Mempool,
    tx_verifier: &mut MockTxVerifier,
    transaction: VerifiedUnminedTx,
) -> bool {
    let queued = mempool
        .ready()
        .await
        .expect("the mempool is ready")
        .call(Request::Queue(vec![Gossip::Tx(
            transaction.transaction.clone(),
        )]));
    let verified = tx_verifier.expect_request_that(|_| true).map(|responder| {
        responder.respond(tx::MempoolResponse {
            transaction,
            spent_mempool_outpoints: Vec::new(),
        });
    });
    let (response, ()) = futures::join!(queued, verified);
    let Ok(Response::Queued(mut results)) = response else {
        panic!("the mempool queues the transaction: {response:?}");
    };
    let mut result = results
        .remove(0)
        .expect("the transaction passes the mempool's first checks");

    // Drive the mempool until it has taken the verified transaction.
    let stored = futures::future::poll_fn(|cx| {
        futures::ready!(mempool.poll_ready(cx)).expect("the mempool has no errors");
        result.poll_unpin(cx)
    });
    tokio::time::timeout(Duration::from_secs(10), stored)
        .await
        .expect("the mempool stores the transaction in time")
        .expect("the mempool answers the queued transaction")
        .is_ok()
}

/// Each time the mempool accepts a transaction that changes the Orchard or Ironwood tree, it
/// names its likeliest such transactions, the likeliest last. Other transactions change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn likeliest_shielded_transactions_are_expected_when_one_is_accepted() {
    let _init_guard = zebra_test::init();

    let (mut mempool, _state, mut tx_verifier, _recent_syncs, mut expected) = setup(u64::MAX).await;
    let (mut orchard, other) = transactions();
    let likelier = orchard.pop().expect("there are two Orchard transactions");
    let less_likely = orchard.pop().expect("there are two Orchard transactions");
    let (likelier_id, less_likely_id) = (likelier.transaction.id, less_likely.transaction.id);

    assert!(accept(&mut mempool, &mut tx_verifier, likelier).await);
    assert_eq!(expected.try_recv(), Ok(vec![likelier_id]));

    // A transaction that changes neither tree leaves the likeliest transactions as they are.
    assert!(accept(&mut mempool, &mut tx_verifier, other).await);
    assert!(expected.try_recv().is_err());

    assert!(accept(&mut mempool, &mut tx_verifier, less_likely).await);
    assert_eq!(expected.try_recv(), Ok(vec![less_likely_id, likelier_id]));

    assert!(expected.try_recv().is_err());
    assert_eq!(mempool.storage().transaction_count(), 3);
}

/// A transaction that is evicted to make room for another one can be one of the likeliest, so
/// the mempool names its likeliest transactions again after an eviction, whichever transaction
/// was accepted and whichever was evicted.
#[tokio::test(flavor = "multi_thread")]
async fn likeliest_shielded_transactions_are_expected_again_after_an_eviction() {
    let _init_guard = zebra_test::init();

    let (mut orchard, other) = transactions();
    let second = orchard.pop().expect("there are two Orchard transactions");
    let first = orchard.pop().expect("there are two Orchard transactions");
    // Room for the two Orchard transactions, and not for a third transaction.
    let tx_cost_limit = first.cost() + second.cost();

    let (mut mempool, _state, mut tx_verifier, _recent_syncs, mut expected) =
        setup(tx_cost_limit).await;
    let likeliest = |mempool: &mut Mempool| -> Vec<_> {
        mempool
            .storage()
            .likeliest_tree_update_transactions(zs::PRECOMPUTED_TREE_UPDATE_TRANSACTIONS)
            .into_iter()
            .map(|tx| tx.id)
            .collect()
    };

    assert!(accept(&mut mempool, &mut tx_verifier, first).await);
    assert!(accept(&mut mempool, &mut tx_verifier, second).await);
    while expected.try_recv().is_ok() {}
    assert_eq!(likeliest(&mut mempool).len(), 2);

    // The mempool evicts one of the three transactions, at random.
    accept(&mut mempool, &mut tx_verifier, other).await;
    assert_eq!(mempool.storage().transaction_count(), 2);
    assert_eq!(expected.try_recv(), Ok(likeliest(&mut mempool)));
}

/// After a new tip, the mempool names its likeliest transactions again: the state computed
/// their tree updates for the tip before.
#[tokio::test(flavor = "multi_thread")]
async fn likeliest_shielded_transactions_are_expected_again_after_a_new_tip() {
    let _init_guard = zebra_test::init();

    let (mut mempool, mut state, _tx_verifier, _recent_syncs, mut expected) = setup(u64::MAX).await;
    let (orchard, other) = transactions();

    let ids: Vec<_> = orchard.iter().map(|tx| tx.transaction.id).collect();
    let other_id = other.transaction.id;
    for transaction in orchard.into_iter().chain([other]) {
        mempool
            .storage()
            .insert(
                transaction,
                Vec::new(),
                Some(Height(0)),
                &mut HashSet::new(),
            )
            .expect("the mempool stores the transaction");
    }
    assert!(expected.try_recv().is_err());

    commit(&mut state, &zebra_test::vectors::BLOCK_MAINNET_1_BYTES).await;
    let named = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            mempool.dummy_call().await;
            if let Ok(named) = expected.try_recv() {
                break named;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the mempool names its transactions after the new tip");

    let limit = zs::PRECOMPUTED_TREE_UPDATE_TRANSACTIONS;
    assert_eq!(named, &ids[ids.len().saturating_sub(limit)..]);
    assert!(mempool
        .storage()
        .transactions()
        .contains_key(&other_id.mined_id()));
    assert!(!named.contains(&other_id));
}

/// The storage picks the likeliest transactions for any limit, by fee weight ratio and then by
/// transaction ID.
#[tokio::test(flavor = "multi_thread")]
async fn likeliest_shielded_transactions_have_the_highest_ratios_then_the_lowest_ids() {
    let _init_guard = zebra_test::init();

    let (mut mempool, _state, _tx_verifier, _recent_syncs, _expected) = setup(u64::MAX).await;
    let (mut orchard, other) = transactions();
    let named = |mempool: &mut Mempool, limit| -> Vec<_> {
        mempool
            .storage()
            .likeliest_tree_update_transactions(limit)
            .into_iter()
            .map(|tx| tx.id)
            .collect()
    };

    // Two transactions with the same ratio are taken in the order of their IDs.
    let last = orchard.len() - 1;
    orchard[last].fee_weight_ratio = orchard[last - 1].fee_weight_ratio;
    let mut tied = [
        orchard[last - 1].transaction.id,
        orchard[last].transaction.id,
    ];
    tied.sort_by_key(|id| std::cmp::Reverse(id.mined_id()));
    let mut ids: Vec<_> = orchard[..last - 1]
        .iter()
        .map(|tx| tx.transaction.id)
        .collect();
    ids.extend(tied);

    for transaction in orchard.into_iter().chain([other]) {
        mempool
            .storage()
            .insert(
                transaction,
                Vec::new(),
                Some(Height(0)),
                &mut HashSet::new(),
            )
            .expect("the mempool stores the transaction");
    }

    for limit in 0..=ids.len() + 1 {
        assert_eq!(
            named(&mut mempool, limit),
            &ids[ids.len().saturating_sub(limit)..],
            "limit {limit}",
        );
    }
}
