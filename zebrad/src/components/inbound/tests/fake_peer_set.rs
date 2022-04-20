//! Inbound service tests with a fake peer set.

use std::{
    collections::HashSet,
    iter::{self, FromIterator},
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use futures::FutureExt;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{buffer::Buffer, builder::ServiceBuilder, util::BoxService, Service, ServiceExt};
use tracing::Span;

use zebra_chain::{
    amount::Amount,
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTx, UnminedTxId, VerifiedUnminedTx},
};
use zebra_consensus::{error::TransactionError, transaction, Config as ConsensusConfig};
use zebra_network::{AddressBook, InventoryResponse, Request, Response};
use zebra_node_services::mempool;
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::{
        inbound::{Inbound, InboundSetupData},
        mempool::{
            gossip_mempool_transaction_id, unmined_transactions_in_blocks, Config as MempoolConfig,
            Mempool, MempoolError, SameEffectsChainRejectionError, UnboxMempoolError,
        },
        sync::{self, BlockGossipError, SyncStatus},
    },
    BoxError,
};

use InventoryResponse::*;

/// Maximum time to wait for a network service request.
///
/// The default [`MockService`] value can be too short for some of these tests that take a little
/// longer than expected to actually send the network request.
///
/// Increasing this value causes the tests to take longer to complete, so it can't be too large.
const MAX_PEER_SET_REQUEST_DELAY: Duration = Duration::from_millis(500);

#[tokio::test]
async fn mempool_requests_for_transactions() {
    let (
        inbound_service,
        _mempool_guard,
        _committed_blocks,
        added_transactions,
        _mock_tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(true).await;

    let added_transactions: Vec<UnminedTx> = added_transactions
        .iter()
        .map(|t| t.transaction.clone())
        .collect();
    let added_transaction_ids: Vec<UnminedTxId> = added_transactions.iter().map(|t| t.id).collect();

    // Test `Request::MempoolTransactionIds`
    let response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;
    match response {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, added_transaction_ids),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`, got {:?}",
            response
        ),
    };

    // Test `Request::TransactionsById`
    let hash_set = added_transaction_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();

    let response = inbound_service
        .clone()
        .oneshot(Request::TransactionsById(hash_set))
        .await;

    match response {
        Ok(Response::Transactions(response)) => {
            assert_eq!(
                response,
                added_transactions
                    .into_iter()
                    .map(Available)
                    .collect::<Vec<_>>(),
            )
        }
        _ => unreachable!("`TransactionsById` requests should always respond `Ok(Vec<UnminedTx>)`"),
    };

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        matches!(sync_gossip_result, None),
        "unexpected error or panic in sync gossip task: {:?}",
        sync_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );
}

#[tokio::test]
async fn mempool_push_transaction() -> Result<(), crate::BoxError> {
    // get a block that has at least one non coinbase transaction
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // use the first transaction that is not coinbase
    let tx = block.transactions[1].clone();

    let (
        inbound_service,
        _mempool_guard,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Test `Request::PushTransaction`
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let transaction = responder
            .request()
            .clone()
            .into_mempool_transaction()
            .expect("unexpected non-mempool request");

        // Set a dummy fee.
        responder.respond(transaction::Response::from(VerifiedUnminedTx::new(
            transaction,
            Amount::zero(),
        )));
    });
    let (response, _) = futures::join!(request, verification);
    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`PushTransaction` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, vec![tx.unmined_id()]),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    // Make sure there is an additional request broadcasting the
    // inserted transaction to peers.
    let mut hs = HashSet::new();
    hs.insert(tx.unmined_id());
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs))
        .await
        .respond(Response::Nil);

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        matches!(sync_gossip_result, None),
        "unexpected error or panic in sync gossip task: {:?}",
        sync_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

#[tokio::test]
async fn mempool_advertise_transaction_ids() -> Result<(), crate::BoxError> {
    // get a block that has at least one non coinbase transaction
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // use the first transaction that is not coinbase
    let test_transaction = block
        .transactions
        .into_iter()
        .find(|tx| !tx.is_coinbase())
        .expect("at least one non-coinbase transaction");
    let test_transaction_id = test_transaction.unmined_id();
    let txs = HashSet::from_iter([test_transaction_id]);

    let (
        inbound_service,
        _mempool_guard,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Test `Request::AdvertiseTransactionIds`
    let request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseTransactionIds(txs.clone()));
    // Ensure the mocked peer set responds
    let peer_set_responder =
        peer_set
            .expect_request(Request::TransactionsById(txs))
            .map(|responder| {
                let unmined_transaction = UnminedTx::from(test_transaction.clone());
                responder.respond(Response::Transactions(vec![Available(unmined_transaction)]))
            });
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let transaction = responder
            .request()
            .clone()
            .into_mempool_transaction()
            .expect("unexpected non-mempool request");

        // Set a dummy fee.
        responder.respond(transaction::Response::from(VerifiedUnminedTx::new(
            transaction,
            Amount::zero(),
        )));
    });
    let (response, _, _) = futures::join!(request, peer_set_responder, verification);

    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`AdvertiseTransactionIds` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => {
            assert_eq!(response, vec![test_transaction_id])
        }
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    // Make sure there is an additional request broadcasting the
    // inserted transaction to peers.
    let mut hs = HashSet::new();
    hs.insert(test_transaction.unmined_id());
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs))
        .await
        .respond(Response::Nil);

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        matches!(sync_gossip_result, None),
        "unexpected error or panic in sync gossip task: {:?}",
        sync_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

#[tokio::test]
async fn mempool_transaction_expiration() -> Result<(), crate::BoxError> {
    // Get a block that has at least one non coinbase transaction
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // Use the first transaction that is not coinbase to test expiration
    let tx1 = &*(block.transactions[1]).clone();
    let mut tx1_id = tx1.unmined_id();

    // Change the expiration height of the transaction to block 3
    let mut tx1 = tx1.clone();
    *tx1.expiry_height_mut() = zebra_chain::block::Height(3);

    // Use the second transaction that is not coinbase to trigger `remove_expired_transactions()`
    let tx2 = block.transactions[2].clone();
    let mut tx2_id = tx2.unmined_id();

    // Get services
    let (
        inbound_service,
        mempool,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        state_service,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Push test transaction
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx1.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        tx1_id = responder.request().tx_id();
        let transaction = responder
            .request()
            .clone()
            .into_mempool_transaction()
            .expect("unexpected non-mempool request");

        // Set a dummy fee.
        responder.respond(transaction::Response::from(VerifiedUnminedTx::new(
            transaction,
            Amount::zero(),
        )));
    });
    let (response, _) = futures::join!(request, verification);
    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`PushTransaction` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => {
            assert_eq!(response, vec![tx1_id])
        }
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    // Add a new block to the state (make the chain tip advance)
    let block_two: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitFinalizedBlock(
            block_two.clone().into(),
        ))
        .await
        .unwrap();

    // Test transaction 1 is gossiped
    let mut hs = HashSet::new();
    hs.insert(tx1_id);
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs))
        .await
        .respond(Response::Nil);

    // Block is gossiped then
    peer_set
        .expect_request(Request::AdvertiseBlock(block_two.hash()))
        .await
        .respond(Response::Nil);

    // Make sure tx1 is still in the mempool as it is not expired yet.
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => {
            assert_eq!(response, vec![tx1_id])
        }
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    // As our test transaction will expire at a block height greater or equal to 3 we need to push block 3.
    let block_three: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitFinalizedBlock(
            block_three.clone().into(),
        ))
        .await
        .unwrap();

    // Block is gossiped
    peer_set
        .expect_request(Request::AdvertiseBlock(block_three.hash()))
        .await
        .respond(Response::Nil);

    // Push a second transaction to trigger `remove_expired_transactions()`
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx2.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        tx2_id = responder.request().tx_id();
        let transaction = responder
            .request()
            .clone()
            .into_mempool_transaction()
            .expect("unexpected non-mempool request");

        // Set a dummy fee.
        responder.respond(transaction::Response::from(VerifiedUnminedTx::new(
            transaction,
            Amount::zero(),
        )));
    });
    let (response, _) = futures::join!(request, verification);
    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`PushTransaction` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    // Only tx2 will be in the mempool while tx1 was expired
    match request {
        Ok(Response::TransactionIds(response)) => {
            assert_eq!(response, vec![tx2_id])
        }
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };
    // Check if tx1 was added to the rejected list as well
    let response = mempool
        .clone()
        .oneshot(mempool::Request::Queue(vec![tx1_id.into()]))
        .await
        .unwrap();

    let queued_responses = match response {
        mempool::Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };

    assert_eq!(queued_responses.len(), 1);
    assert_eq!(
        queued_responses
            .into_iter()
            .next()
            .unwrap()
            .unbox_mempool_error(),
        MempoolError::StorageEffectsChain(SameEffectsChainRejectionError::Expired)
    );

    // Test transaction 2 is gossiped
    let mut hs = HashSet::new();
    hs.insert(tx2_id);
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs))
        .await
        .respond(Response::Nil);

    // Add all the rest of the continuous blocks we have to test tx2 will never expire.
    let more_blocks: Vec<Arc<Block>> = vec![
        zebra_test::vectors::BLOCK_MAINNET_4_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_5_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_6_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_7_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_8_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_9_BYTES
            .zcash_deserialize_into()
            .unwrap(),
        zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap(),
    ];
    for block in more_blocks {
        state_service
            .clone()
            .oneshot(zebra_state::Request::CommitFinalizedBlock(
                block.clone().into(),
            ))
            .await
            .unwrap();

        // Block is gossiped
        peer_set
            .expect_request(Request::AdvertiseBlock(block.hash()))
            .await
            .respond(Response::Nil);

        let request = inbound_service
            .clone()
            .oneshot(Request::MempoolTransactionIds)
            .await;

        // tx2 is still in the mempool as the blockchain progress because the zero expiration height
        match request {
            Ok(Response::TransactionIds(response)) => {
                assert_eq!(response, vec![tx2_id])
            }
            _ => unreachable!(
                "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
            ),
        };
    }

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        matches!(sync_gossip_result, None),
        "unexpected error or panic in sync gossip task: {:?}",
        sync_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Test that the inbound downloader rejects blocks above the lookahead limit.
///
/// TODO: also test that it rejects blocks behind the tip limit. (Needs ~100 fake blocks.)
#[tokio::test]
async fn inbound_block_height_lookahead_limit() -> Result<(), crate::BoxError> {
    // Get services
    let (
        inbound_service,
        _mempool,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        state_service,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Get the next block
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block_hash = block.hash();

    // Push test block hash
    let _request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseBlock(block_hash))
        .await?;

    // Block is fetched, and committed to the state
    peer_set
        .expect_request(Request::BlocksByHash(iter::once(block_hash).collect()))
        .await
        .respond(Response::Blocks(vec![Available(block)]));

    // TODO: check that the block is queued in the checkpoint verifier

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;
    tx_verifier.expect_no_requests().await;

    // Get a block that is a long way away from genesis
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;
    let block_hash = block.hash();

    // Push test block hash
    let _request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseBlock(block_hash))
        .await?;

    // Block is fetched, but the downloader drops it because it is too high
    peer_set
        .expect_request(Request::BlocksByHash(iter::once(block_hash).collect()))
        .await
        .respond(Response::Blocks(vec![Available(block)]));

    let response = state_service
        .clone()
        .oneshot(zebra_state::Request::Depth(block_hash))
        .await?;
    assert_eq!(response, zebra_state::Response::Depth(None));

    // TODO: check that the block is not queued in the checkpoint verifier or non-finalized state

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;
    tx_verifier.expect_no_requests().await;

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        matches!(sync_gossip_result, None),
        "unexpected error or panic in sync gossip task: {:?}",
        sync_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Setup a fake Zebra network stack, with fake peer set.
///
/// Adds some initial state blocks, and mempool transactions if `add_transactions` is true.
///
/// Uses a real block verifier, but a fake transaction verifier.
/// Does not run a block syncer task.
async fn setup(
    add_transactions: bool,
) -> (
    Buffer<
        BoxService<zebra_network::Request, zebra_network::Response, BoxError>,
        zebra_network::Request,
    >,
    Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>,
    Vec<Arc<Block>>,
    Vec<VerifiedUnminedTx>,
    MockService<transaction::Request, transaction::Response, PanicAssertion, TransactionError>,
    MockService<Request, Response, PanicAssertion>,
    Buffer<BoxService<zebra_state::Request, zebra_state::Response, BoxError>, zebra_state::Request>,
    JoinHandle<Result<(), BlockGossipError>>,
    JoinHandle<Result<(), BoxError>>,
) {
    zebra_test::init();

    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Span::none());
    let address_book = Arc::new(std::sync::Mutex::new(address_book));
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (state, _read_only_state_service, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let mut state_service = ServiceBuilder::new().buffer(1).service(state);

    // Download task panics and timeouts are propagated to the tests that use Groth16 verifiers.
    let (block_verifier, _transaction_verifier, _groth16_download_handle) =
        zebra_consensus::chain::init(
            consensus_config.clone(),
            network,
            state_service.clone(),
            true,
        )
        .await;

    let mut peer_set = MockService::build()
        .with_max_request_delay(MAX_PEER_SET_REQUEST_DELAY)
        .for_unit_tests();
    let buffered_peer_set = Buffer::new(BoxService::new(peer_set.clone()), 10);

    let mock_tx_verifier = MockService::build().for_unit_tests();
    let buffered_tx_verifier = Buffer::new(BoxService::new(mock_tx_verifier.clone()), 10);

    let mut committed_blocks = Vec::new();

    // Push the genesis block to the state.
    // This must be done before creating the mempool to avoid `chain_tip_change`
    // returning "reset" which would clear the mempool.
    let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitFinalizedBlock(
            genesis_block.clone().into(),
        ))
        .await
        .unwrap();
    committed_blocks.push(genesis_block);

    // Also push block 1.
    // Block one is a network upgrade and the mempool will be cleared at it,
    // let all our tests start after this event.
    let block_one: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitFinalizedBlock(
            block_one.clone().into(),
        ))
        .await
        .unwrap();
    committed_blocks.push(block_one);

    let (mut mempool_service, transaction_receiver) = Mempool::new(
        &MempoolConfig::default(),
        buffered_peer_set.clone(),
        state_service.clone(),
        buffered_tx_verifier.clone(),
        sync_status.clone(),
        latest_chain_tip.clone(),
        chain_tip_change.clone(),
    );

    // Enable the mempool
    mempool_service.enable(&mut recent_syncs).await;

    // Add transactions to the mempool, skipping verification and broadcast
    let mut added_transactions = Vec::new();
    if add_transactions {
        added_transactions.extend(add_some_stuff_to_mempool(&mut mempool_service, network));
    }

    let mempool_service = BoxService::new(mempool_service);
    let mempool_service = ServiceBuilder::new().buffer(1).service(mempool_service);

    let (setup_tx, setup_rx) = oneshot::channel();

    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .service(Inbound::new(setup_rx));
    let inbound_service = BoxService::new(inbound_service);
    let inbound_service = ServiceBuilder::new().buffer(1).service(inbound_service);

    let setup_data = InboundSetupData {
        address_book,
        block_download_peer_set: buffered_peer_set,
        block_verifier,
        mempool: mempool_service.clone(),
        state: state_service.clone(),
        latest_chain_tip,
    };
    let r = setup_tx.send(setup_data);
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok(), "unexpected setup channel send failure");

    let sync_gossip_task_handle = tokio::spawn(sync::gossip_best_tip_block_hashes(
        sync_status.clone(),
        chain_tip_change,
        peer_set.clone(),
    ));

    let tx_gossip_task_handle = tokio::spawn(gossip_mempool_transaction_id(
        transaction_receiver,
        peer_set.clone(),
    ));

    // Make sure there is an additional request broadcasting the
    // committed blocks to peers.
    //
    // (The genesis block gets skipped, because block 1 is committed before the task is spawned.)
    for block in committed_blocks.iter().skip(1) {
        peer_set
            .expect_request(Request::AdvertiseBlock(block.hash()))
            .await
            .respond(Response::Nil);
    }

    (
        inbound_service,
        mempool_service,
        committed_blocks,
        added_transactions,
        mock_tx_verifier,
        peer_set,
        state_service,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    )
}

/// Manually add a transaction to the mempool storage.
///
/// Skips some mempool functionality, like transaction verification and peer broadcasts.
fn add_some_stuff_to_mempool(
    mempool_service: &mut Mempool,
    network: Network,
) -> Vec<VerifiedUnminedTx> {
    // get the genesis block coinbase transaction from the Zcash blockchain.
    let genesis_transactions: Vec<_> = unmined_transactions_in_blocks(..=0, network)
        .take(1)
        .collect();

    // Insert the genesis block coinbase transaction into the mempool storage.
    mempool_service
        .storage()
        .insert(genesis_transactions[0].clone())
        .unwrap();

    genesis_transactions
}
