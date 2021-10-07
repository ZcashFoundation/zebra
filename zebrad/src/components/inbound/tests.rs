use std::{collections::HashSet, iter::FromIterator, net::SocketAddr, str::FromStr, sync::Arc};

use futures::FutureExt;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{
    buffer::Buffer, builder::ServiceBuilder, load_shed::LoadShed, util::BoxService, Service,
    ServiceExt,
};
use tracing::Span;

use zebra_chain::{
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::{error::TransactionError, transaction, Config as ConsensusConfig};
use zebra_network::{AddressBook, Request, Response};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::components::{
    mempool::{gossip_mempool_transaction_id, unmined_transactions_in_blocks, Mempool},
    sync::{self, BlockGossipError, SyncStatus},
};

#[tokio::test]
async fn mempool_requests_for_transactions() {
    let (
        inbound_service,
        _committed_blocks,
        added_transactions,
        _mock_tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
    ) = setup(true).await;

    let added_transaction_ids: Vec<UnminedTxId> = added_transactions.iter().map(|t| t.id).collect();

    // Test `Request::MempoolTransactionIds`
    let response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;
    match response {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, added_transaction_ids),
        _ => unreachable!(format!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`, got {:?}",
            response
        )),
    };

    // Test `Request::TransactionsById`
    let hash_set = added_transaction_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();

    let response = inbound_service
        .oneshot(Request::TransactionsById(hash_set))
        .await;

    match response {
        Ok(Response::Transactions(response)) => assert_eq!(response, added_transactions),
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
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
    ) = setup(false).await;

    // Test `Request::PushTransaction`
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let txid = responder.request().tx_id();
        responder.respond(txid);
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
        .find(|tx| !tx.has_any_coinbase_inputs())
        .expect("at least one non-coinbase transaction");
    let test_transaction_id = test_transaction.unmined_id();
    let txs = HashSet::from_iter([test_transaction_id]);

    let (
        inbound_service,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        _state_guard,
        sync_gossip_task_handle,
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
                responder.respond(Response::Transactions(vec![unmined_transaction]))
            });
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let txid = responder.request().tx_id();
        responder.respond(txid);
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
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        state_service,
        sync_gossip_task_handle,
    ) = setup(false).await;

    // Push test transaction
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx1.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        tx1_id = responder.request().tx_id();
        responder.respond(tx1_id);
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
        responder.respond(tx2_id);
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

    // Test transaction 2 is gossiped
    let mut hs = HashSet::new();
    hs.insert(tx2_id);
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs))
        .await
        .respond(Response::Nil);

    // Add all the rest of the continous blocks we have to test tx2 will never expire.
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

    Ok(())
}

async fn setup(
    add_transactions: bool,
) -> (
    LoadShed<tower::buffer::Buffer<super::Inbound, zebra_network::Request>>,
    Vec<Arc<Block>>,
    Vec<UnminedTx>,
    MockService<transaction::Request, transaction::Response, PanicAssertion, TransactionError>,
    MockService<Request, Response, PanicAssertion>,
    Buffer<
        BoxService<
            zebra_state::Request,
            zebra_state::Response,
            Box<dyn std::error::Error + Send + Sync>,
        >,
        zebra_state::Request,
    >,
    JoinHandle<Result<(), BlockGossipError>>,
) {
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Span::none());
    let address_book = Arc::new(std::sync::Mutex::new(address_book));
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (state, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let mut state_service = ServiceBuilder::new().buffer(1).service(state);

    let (block_verifier, _transaction_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    let mut peer_set = MockService::build().for_unit_tests();
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
        .ready_and()
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

    let (transaction_sender, transaction_receiver) = tokio::sync::watch::channel(HashSet::new());

    let mut mempool_service = Mempool::new(
        buffered_peer_set.clone(),
        state_service.clone(),
        buffered_tx_verifier.clone(),
        sync_status.clone(),
        latest_chain_tip,
        chain_tip_change.clone(),
        transaction_sender,
    );

    // Enable the mempool
    mempool_service.enable(&mut recent_syncs).await;

    let _ = tokio::spawn(gossip_mempool_transaction_id(
        transaction_receiver,
        peer_set.clone(),
    ));

    let mut added_transactions = Vec::new();
    if add_transactions {
        added_transactions.extend(add_some_stuff_to_mempool(&mut mempool_service, network));
    }

    let mempool_service = BoxService::new(mempool_service);
    let mempool = ServiceBuilder::new().buffer(1).service(mempool_service);

    let (setup_tx, setup_rx) = oneshot::channel();

    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .buffer(1)
        .service(super::Inbound::new(
            setup_rx,
            state_service.clone(),
            block_verifier.clone(),
        ));

    let r = setup_tx.send((buffered_peer_set, address_book, mempool));
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok(), "unexpected setup channel send failure");

    let sync_gossip_task_handle = tokio::spawn(sync::gossip_best_tip_block_hashes(
        sync_status.clone(),
        chain_tip_change,
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
        committed_blocks,
        added_transactions,
        mock_tx_verifier,
        peer_set,
        state_service,
        sync_gossip_task_handle,
    )
}

fn add_some_stuff_to_mempool(mempool_service: &mut Mempool, network: Network) -> Vec<UnminedTx> {
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
