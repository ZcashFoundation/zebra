//! Inbound service tests with a fake peer set.

#![allow(clippy::unwrap_in_result)]

use std::{collections::HashSet, iter, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};
use tower::{buffer::Buffer, builder::ServiceBuilder, util::BoxService, Service, ServiceExt};
use tracing::{Instrument, Span};

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    fmt::humantime_seconds,
    parameters::Network::{self, *},
    serialization::{DateTime32, ZcashDeserializeInto},
    transaction::{UnminedTx, UnminedTxId, VerifiedUnminedTx},
};
use zebra_consensus::{
    error::TransactionError, router::RouterError, transaction, Config as ConsensusConfig,
    VerifyBlockError,
};
use zebra_network::{
    constants::{
        ADDR_RESPONSE_LIMIT_DENOMINATOR, DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK,
    },
    types::{MetaAddr, PeerServices},
    AddressBook, InventoryResponse, PeerSocketAddr, Request, Response,
};
use zebra_node_services::mempool;
use zebra_rpc::SubmitBlockChannel;
use zebra_state::{ChainTipChange, Config as StateConfig, CHAIN_TIP_UPDATE_WAIT_LIMIT};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::{
        inbound::{downloads::MAX_INBOUND_CONCURRENCY, Inbound, InboundSetupData},
        mempool::{
            gossip_mempool_transaction_id, Config as MempoolConfig, Mempool, MempoolError,
            SameEffectsChainRejectionError, UnboxMempoolError,
        },
        sync::{self, BlockGossipError, SyncStatus, BLOCK_VERIFY_TIMEOUT, PEER_GOSSIP_DELAY},
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

#[tokio::test(flavor = "multi_thread")]
async fn mempool_requests_for_transactions() {
    let (
        inbound_service,
        _mempool_guard,
        _committed_blocks,
        added_transactions,
        _mock_tx_verifier,
        mut peer_set,
        _state_guard,
        _chain_tip_change,
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
        Ok(Response::Nil) => assert!(
            added_transaction_ids.is_empty(),
            "`MempoolTransactionIds` request should match added_transaction_ids {added_transaction_ids:?}, got Ok(Nil)"
        ),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`, got {:?}",
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
                    .map(|tx| Available((tx, None)))
                    .collect::<Vec<_>>(),
            )
        }
        _ => unreachable!("`TransactionsById` requests should always respond `Ok(Vec<UnminedTx>)`"),
    };

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        sync_gossip_result.is_none(),
        "unexpected error or panic in sync gossip task: {sync_gossip_result:?}",
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        tx_gossip_result.is_none(),
        "unexpected error or panic in transaction gossip task: {tx_gossip_result:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mempool_push_transaction() -> Result<(), crate::BoxError> {
    // get a block that has at least one non coinbase transaction
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // use the first transaction that is not coinbase
    let tx = block.transactions[1].clone();
    let test_transaction_id = tx.unmined_id();

    let (
        inbound_service,
        _mempool_guard,
        _committed_blocks,
        _added_transactions,
        mut tx_verifier,
        mut peer_set,
        _state_guard,
        _chain_tip_change,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Test `Request::PushTransaction`
    let request = inbound_service.clone().oneshot(Request::PushTransaction(
        tx.clone().into(),
        Some(PeerSocketAddr::from(([192, 168, 180, 9], 10_000))),
    ));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let transaction = responder.request().clone().transaction;

        // Set a dummy fee and sigops.
        responder.respond(transaction::MempoolResponse::from(
            VerifiedUnminedTx::new(
                transaction,
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
                0,
                std::sync::Arc::new(vec![]),
            )
            .expect("verification should pass"),
        ));
    });

    let (push_response, _) = futures::join!(request, verification);

    assert_eq!(
        push_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "`PushTransaction` requests should always respond `Ok(Nil)`",
    );

    // Wait for the mempool to store the transaction
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let mempool_response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    assert_eq!(
        mempool_response.expect("unexpected error response from mempool"),
        Response::TransactionIds(vec![test_transaction_id]),
        "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`",
    );

    // Make sure there is an additional request broadcasting the
    // inserted transaction to peers.
    let mut hs = HashSet::new();
    hs.insert(tx.unmined_id());
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs, None))
        .await
        .respond(Response::Nil);

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        sync_gossip_result.is_none(),
        "unexpected error or panic in sync gossip task: {sync_gossip_result:?}",
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        tx_gossip_result.is_none(),
        "unexpected error or panic in transaction gossip task: {tx_gossip_result:?}",
    );

    Ok(())
}

/// The inbound service must route a directly pushed transaction that carries a
/// sending peer through the per-peer-capped mempool path
/// ([`mempool::Request::QueueFromPeer`]), and only fall back to the uncapped
/// [`mempool::Request::Queue`] path for a source-less local/internal push.
///
/// Routing a peer push through `Queue` was the actual bypass: it dropped the
/// peer address, so the downloader's per-peer cap never applied. The downloader
/// already enforced the cap for source-tagged candidates before this fix, so the
/// regression lives in this routing decision, not in the downloader.
///
/// Regression test for `GHSA-m9xx-8rcj-vmgp`.
#[tokio::test(flavor = "multi_thread")]
async fn push_transaction_routing_enforces_per_peer_source() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();

    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );
    let address_book = Arc::new(std::sync::Mutex::new(address_book));

    // The push path only touches the mempool; the state and block verifier are
    // wired up because `Inbound` requires them, but are never driven here.
    let (state, _read_only_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::init(state_config, &network, Height::MAX, 0).await;
    let state_service = ServiceBuilder::new().buffer(1).service(state);

    let (block_verifier, _transaction_verifier, _groth16_download_handle, _max_checkpoint_height) =
        zebra_consensus::router::init_test(consensus_config, &network, state_service.clone()).await;

    let peer_set = MockService::build()
        .with_max_request_delay(MAX_PEER_SET_REQUEST_DELAY)
        .for_unit_tests();
    let buffered_peer_set = Buffer::new(BoxService::new(peer_set), 10);

    // Keep a handle to the mock mempool so we can assert on the request the
    // inbound service routes to it.
    let mut mempool_service: MockService<mempool::Request, mempool::Response, PanicAssertion> =
        MockService::build()
            .with_max_request_delay(MAX_PEER_SET_REQUEST_DELAY)
            .for_unit_tests();
    let buffered_mempool = Buffer::new(BoxService::new(mempool_service.clone()), 10);

    let (setup_tx, setup_rx) = oneshot::channel();
    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .service(Inbound::new(MAX_INBOUND_CONCURRENCY, setup_rx));
    let inbound_service = BoxService::new(inbound_service);
    let inbound_service = ServiceBuilder::new().buffer(1).service(inbound_service);

    let (misbehavior_sender, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let setup_data = InboundSetupData {
        address_book,
        block_download_peer_set: buffered_peer_set,
        block_verifier,
        mempool: buffered_mempool,
        state: state_service,
        latest_chain_tip,
        misbehavior_sender,
    };
    let r = setup_tx.send(setup_data);
    // We can't expect or unwrap because the returned Result does not implement Debug.
    assert!(r.is_ok(), "unexpected setup channel send failure");

    // A non-coinbase transaction to push.
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;
    let unmined_tx: UnminedTx = block.transactions[1].clone().into();

    // A push tagged with a sending peer is routed through the per-peer-capped path.
    let peer = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(unmined_tx.clone(), Some(peer)));
    let expected = mempool_service
        .expect_request(mempool::Request::QueueFromPeer {
            candidates: vec![mempool::Gossip::Tx(unmined_tx.clone())],
            source: *peer,
        })
        .map(|responder| responder.respond(mempool::Response::Queued(vec![])));
    let (push_response, _) = futures::join!(request, expected);
    assert_eq!(
        push_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "a peer-sourced push must be accepted and routed to `QueueFromPeer`",
    );

    // A source-less push (local/internal) stays on the uncapped `Queue` path.
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(unmined_tx.clone(), None));
    let expected = mempool_service
        .expect_request(mempool::Request::Queue(vec![mempool::Gossip::Tx(
            unmined_tx,
        )]))
        .map(|responder| responder.respond(mempool::Response::Queued(vec![])));
    let (push_response, _) = futures::join!(request, expected);
    assert_eq!(
        push_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "a source-less push must be accepted and routed to `Queue`",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
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
        _chain_tip_change,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Test `Request::AdvertiseTransactionIds`
    let request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseTransactionIds(txs.clone(), None));
    // Ensure the mocked peer set responds
    let peer_set_responder =
        peer_set
            .expect_request(Request::TransactionsById(txs))
            .map(|responder| {
                let unmined_transaction = UnminedTx::from(test_transaction.clone());
                responder.respond(Response::Transactions(vec![Available((
                    unmined_transaction,
                    None,
                ))]))
            });
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let transaction = responder.request().clone().transaction;

        // Set a dummy fee and sigops.
        responder.respond(transaction::MempoolResponse::from(
            VerifiedUnminedTx::new(
                transaction,
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
                0,
                std::sync::Arc::new(vec![]),
            )
            .expect("verification should pass"),
        ));
    });

    let (advertise_response, _, _) = futures::join!(request, peer_set_responder, verification);

    assert_eq!(
        advertise_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "`AdvertiseTransactionIds` requests should always respond `Ok(Nil)`",
    );

    // Wait for the mempool to store the transaction
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let mempool_response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    assert_eq!(
        mempool_response.expect("unexpected error response from mempool"),
        Response::TransactionIds(vec![test_transaction_id]),
        "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`",
    );

    // Make sure there is an additional request broadcasting the
    // inserted transaction to peers.
    let mut hs = HashSet::new();
    hs.insert(test_transaction.unmined_id());
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs, None))
        .await
        .respond(Response::Nil);

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        sync_gossip_result.is_none(),
        "unexpected error or panic in sync gossip task: {sync_gossip_result:?}",
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        tx_gossip_result.is_none(),
        "unexpected error or panic in transaction gossip task: {tx_gossip_result:?}",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
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
        _chain_tip_change,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Push test transaction
    let request = inbound_service.clone().oneshot(Request::PushTransaction(
        tx1.clone().into(),
        Some(PeerSocketAddr::from(([192, 168, 180, 9], 10_000))),
    ));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        tx1_id = responder.request().transaction.id;
        let transaction = responder.request().clone().transaction;

        // Set a dummy fee and sigops.
        responder.respond(transaction::MempoolResponse::from(
            VerifiedUnminedTx::new(
                transaction,
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
                0,
                std::sync::Arc::new(vec![]),
            )
            .expect("verification should pass"),
        ));
    });

    let (push_response, _) = futures::join!(request, verification);

    assert_eq!(
        push_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "`PushTransaction` requests should always respond `Ok(Nil)`",
    );

    // Wait for the mempool to store the transaction
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let mempool_response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    assert_eq!(
        mempool_response.expect("unexpected error response from mempool"),
        Response::TransactionIds(vec![tx1_id]),
        "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`",
    );

    // Add a new block to the state (make the chain tip advance)
    let block_two: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block_two.clone().into(),
        ))
        .await
        .unwrap();

    // Test transaction 1 is gossiped
    let mut hs = HashSet::new();
    hs.insert(tx1_id);

    // Transaction and Block IDs are gossipped, in any order, after waiting for the gossip delay
    tokio::time::sleep(PEER_GOSSIP_DELAY).await;
    let possible_requests = &mut [
        Request::AdvertiseTransactionIds(hs, None),
        Request::AdvertiseBlock(block_two.hash(), None),
    ]
    .to_vec();

    peer_set
        .expect_request_that(|request| {
            let is_possible = possible_requests.contains(request);

            *possible_requests = possible_requests
                .clone()
                .into_iter()
                .filter(|possible| possible != request)
                .collect();

            is_possible
        })
        .await
        .respond(Response::Nil);

    peer_set
        .expect_request_that(|request| {
            let is_possible = possible_requests.contains(request);

            *possible_requests = possible_requests
                .clone()
                .into_iter()
                .filter(|possible| possible != request)
                .collect();

            is_possible
        })
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
        Ok(Response::Nil) => panic!(
            "response to `MempoolTransactionIds` request should match {:?}",
            vec![tx1_id]
        ),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`"
        ),
    };

    // As our test transaction will expire at a block height greater or equal to 3 we need to push block 3.
    let block_three: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block_three.clone().into(),
        ))
        .await
        .unwrap();

    // Test the block is gossiped, after waiting for the multi-gossip delay
    tokio::time::sleep(PEER_GOSSIP_DELAY).await;
    peer_set
        .expect_request(Request::AdvertiseBlock(block_three.hash(), None))
        .await
        .respond(Response::Nil);

    // Push a second transaction to trigger `remove_expired_transactions()`
    let request = inbound_service.clone().oneshot(Request::PushTransaction(
        tx2.clone().into(),
        Some(PeerSocketAddr::from(([192, 168, 180, 9], 10_000))),
    ));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        tx2_id = responder.request().transaction.id;
        let transaction = responder.request().clone().transaction;

        // Set a dummy fee and sigops.
        responder.respond(transaction::MempoolResponse::from(
            VerifiedUnminedTx::new(
                transaction,
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
                0,
                std::sync::Arc::new(vec![]),
            )
            .expect("verification should pass"),
        ));
    });

    let (push_response, _) = futures::join!(request, verification);

    assert_eq!(
        push_response.expect("unexpected error response from inbound service"),
        Response::Nil,
        "`PushTransaction` requests should always respond `Ok(Nil)`",
    );

    // Wait for the mempool to store the transaction
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let mempool_response = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    // Only tx2 will be in the mempool while tx1 was expired
    assert_eq!(
        mempool_response.expect("unexpected error response from mempool"),
        Response::TransactionIds(vec![tx2_id]),
        "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`",
    );

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

    // Test transaction 2 is gossiped, after waiting for the multi-gossip delay
    tokio::time::sleep(PEER_GOSSIP_DELAY).await;

    let mut hs = HashSet::new();
    hs.insert(tx2_id);
    peer_set
        .expect_request(Request::AdvertiseTransactionIds(hs, None))
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
    ];
    for block in more_blocks {
        state_service
            .clone()
            .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
                block.clone().into(),
            ))
            .await
            .unwrap();

        // Test the block is gossiped, after waiting for the multi-gossip delay
        tokio::time::sleep(PEER_GOSSIP_DELAY).await;
        peer_set
            .expect_request(Request::AdvertiseBlock(block.hash(), None))
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
            Ok(Response::Nil) => panic!(
                "response to `MempoolTransactionIds` request should match {:?}",
                vec![tx2_id]
            ),
            _ => unreachable!(
                "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId> | Nil)`"
            ),
        };
    }

    // check that nothing unexpected happened
    peer_set.expect_no_requests().await;

    let sync_gossip_result = sync_gossip_task_handle.now_or_never();
    assert!(
        sync_gossip_result.is_none(),
        "unexpected error or panic in sync gossip task: {sync_gossip_result:?}",
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        tx_gossip_result.is_none(),
        "unexpected error or panic in transaction gossip task: {tx_gossip_result:?}",
    );

    Ok(())
}

/// Test that the inbound downloader rejects blocks above the lookahead limit.
///
/// TODO: also test that it rejects blocks behind the tip limit. (Needs ~100 fake blocks.)
#[tokio::test(flavor = "multi_thread")]
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
        mut chain_tip_change,
        sync_gossip_task_handle,
        tx_gossip_task_handle,
    ) = setup(false).await;

    // Get the next block
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block_hash = block.hash();

    // Push test block hash
    let _request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseBlock(block_hash, None))
        .await?;

    // Block is fetched, and committed to the state
    peer_set
        .expect_request(Request::BlocksByHash(iter::once(block_hash).collect()))
        .await
        .respond(Response::Blocks(vec![Available((block, None))]));

    // Wait for the chain tip update
    if let Err(timeout_error) = timeout(
        CHAIN_TIP_UPDATE_WAIT_LIMIT,
        chain_tip_change.wait_for_tip_change(),
    )
    .await
    .map(|change_result| change_result.expect("unexpected chain tip update failure"))
    {
        info!(
            timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
            ?timeout_error,
            "timeout waiting for chain tip change after committing block"
        );
    }

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
        .oneshot(Request::AdvertiseBlock(block_hash, None))
        .await?;

    // Block is fetched, but the downloader drops it because it is too high
    peer_set
        .expect_request(Request::BlocksByHash(iter::once(block_hash).collect()))
        .await
        .respond(Response::Blocks(vec![Available((block, None))]));

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
        sync_gossip_result.is_none(),
        "unexpected error or panic in sync gossip task: {sync_gossip_result:?}",
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        tx_gossip_result.is_none(),
        "unexpected error or panic in transaction gossip task: {tx_gossip_result:?}",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
/// Checks that Zebra won't give out its entire address book over a short duration.
async fn caches_getaddr_response() {
    const NUM_ADDRESSES: usize = 20;
    const NUM_REQUESTS: usize = 10;
    const EXPECTED_NUM_RESULTS: usize = NUM_ADDRESSES / ADDR_RESPONSE_LIMIT_DENOMINATOR;

    let _init_guard = zebra_test::init();

    let addrs = (0..NUM_ADDRESSES)
        .map(|idx| format!("127.0.0.{idx}:{idx}"))
        .map(|addr| {
            MetaAddr::new_gossiped_meta_addr(
                addr.parse().unwrap(),
                PeerServices::NODE_NETWORK,
                DateTime32::now(),
            )
        });

    let inbound = {
        let network = Mainnet;
        let consensus_config = ConsensusConfig::default();
        let state_config = StateConfig::ephemeral();
        let address_book = AddressBook::new_with_addrs(
            SocketAddr::from_str("0.0.0.0:0").unwrap(),
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            addrs,
        );

        let address_book = Arc::new(std::sync::Mutex::new(address_book));

        // UTXO verification doesn't matter for these tests.
        let (state, _read_only_state_service, latest_chain_tip, _chain_tip_change) =
            zebra_state::init(state_config.clone(), &network, Height::MAX, 0).await;

        let state_service = ServiceBuilder::new().buffer(1).service(state);

        // Download task panics and timeouts are propagated to the tests that use Groth16 verifiers.
        let (
            block_verifier,
            _transaction_verifier,
            _groth16_download_handle,
            _max_checkpoint_height,
        ) = zebra_consensus::router::init_test(
            consensus_config.clone(),
            &network,
            state_service.clone(),
        )
        .await;

        let peer_set = MockService::build()
            .with_max_request_delay(MAX_PEER_SET_REQUEST_DELAY)
            .for_unit_tests();
        let buffered_peer_set = Buffer::new(BoxService::new(peer_set.clone()), 10);

        let buffered_mempool_service =
            Buffer::new(BoxService::new(MockService::build().for_unit_tests()), 10);
        let (setup_tx, setup_rx) = oneshot::channel();

        let inbound_service = ServiceBuilder::new()
            .load_shed()
            .service(Inbound::new(MAX_INBOUND_CONCURRENCY, setup_rx));
        let inbound_service = BoxService::new(inbound_service);
        let inbound_service = ServiceBuilder::new().buffer(1).service(inbound_service);
        let (misbehavior_sender, _misbehavior_rx) = tokio::sync::mpsc::channel(1);

        let setup_data = InboundSetupData {
            address_book: address_book.clone(),
            block_download_peer_set: buffered_peer_set,
            block_verifier,
            mempool: buffered_mempool_service.clone(),
            state: state_service.clone(),
            latest_chain_tip,
            misbehavior_sender,
        };
        let r = setup_tx.send(setup_data);
        // We can't expect or unwrap because the returned Result does not implement Debug
        assert!(r.is_ok(), "unexpected setup channel send failure");

        inbound_service
    };

    let Ok(zebra_network::Response::Peers(first_result)) =
        inbound.clone().oneshot(zebra_network::Request::Peers).await
    else {
        panic!("result should match Ok(Peers(_))")
    };

    assert_eq!(
        first_result.len(),
        EXPECTED_NUM_RESULTS,
        "inbound service should respond with expected number of peer addresses",
    );

    for _ in 0..NUM_REQUESTS {
        let Ok(zebra_network::Response::Peers(peers)) =
            inbound.clone().oneshot(zebra_network::Request::Peers).await
        else {
            panic!("result should match Ok(Peers(_))")
        };

        assert_eq!(
            peers, first_result,
            "inbound service should return the same result for every Peers request until the refresh time",
        );
    }
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
    MockService<
        transaction::MempoolRequest,
        transaction::MempoolResponse,
        PanicAssertion,
        TransactionError,
    >,
    MockService<Request, Response, PanicAssertion>,
    Buffer<BoxService<zebra_state::Request, zebra_state::Response, BoxError>, zebra_state::Request>,
    ChainTipChange,
    JoinHandle<Result<(), BlockGossipError>>,
    JoinHandle<Result<(), BoxError>>,
) {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );
    let address_book = Arc::new(std::sync::Mutex::new(address_book));
    let (sync_status, mut recent_syncs) = SyncStatus::new();

    // UTXO verification doesn't matter for these tests.
    let (state, _read_only_state_service, latest_chain_tip, mut chain_tip_change) =
        zebra_state::init(state_config.clone(), &network, Height::MAX, 0).await;

    let mut state_service = ServiceBuilder::new().buffer(1).service(state);

    // Download task panics and timeouts are propagated to the tests that use Groth16 verifiers.
    let (block_verifier, _transaction_verifier, _groth16_download_handle, _max_checkpoint_height) =
        zebra_consensus::router::init_test(
            consensus_config.clone(),
            &network,
            state_service.clone(),
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
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            genesis_block.clone().into(),
        ))
        .await
        .unwrap();
    committed_blocks.push(genesis_block);

    // Wait for the chain tip update
    if let Err(timeout_error) = timeout(
        CHAIN_TIP_UPDATE_WAIT_LIMIT,
        chain_tip_change.wait_for_tip_change(),
    )
    .await
    .map(|change_result| change_result.expect("unexpected chain tip update failure"))
    {
        info!(
            timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
            ?timeout_error,
            "timeout waiting for chain tip change after committing block"
        );
    }

    // Also push block 1.
    // Block one is a network upgrade and the mempool will be cleared at it,
    // let all our tests start after this event.
    let block_one: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .clone()
        .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block_one.clone().into(),
        ))
        .await
        .unwrap();
    committed_blocks.push(block_one);

    // Don't wait for the chain tip update here, we wait for expect_request(AdvertiseBlock) below,
    // which is called by the gossip_best_tip_block_hashes task once the chain tip changes.

    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mut mempool_service, transaction_subscriber) = Mempool::new(
        &network,
        &MempoolConfig::default(),
        buffered_peer_set.clone(),
        state_service.clone(),
        buffered_tx_verifier.clone(),
        sync_status.clone(),
        latest_chain_tip.clone(),
        chain_tip_change.clone(),
        misbehavior_tx,
    );

    // Pretend we're close to tip
    SyncStatus::sync_close_to_tip(&mut recent_syncs);

    let submitblock_channel = SubmitBlockChannel::new();
    let sync_gossip_task_handle = tokio::spawn(
        sync::gossip_best_tip_block_hashes(
            sync_status.clone(),
            chain_tip_change.clone(),
            peer_set.clone(),
            Some(submitblock_channel.receiver()),
        )
        .in_current_span(),
    );

    let tx_gossip_task_handle = tokio::spawn(gossip_mempool_transaction_id(
        transaction_subscriber.subscribe(),
        peer_set.clone(),
    ));

    // Make sure there is an additional request broadcasting the
    // committed blocks to peers.
    //
    // (The genesis block gets skipped, because block 1 is committed before the task is spawned.)
    for block in committed_blocks.iter().skip(1) {
        tokio::time::sleep(PEER_GOSSIP_DELAY).await;

        peer_set
            .expect_request(Request::AdvertiseBlock(block.hash(), None))
            .await
            .respond(Response::Nil);
    }

    // Enable the mempool
    // Note: this needs to be done after the mock peer set service has received the AdvertiseBlock
    // request to ensure that the call to `last_tip_change` returns the chain tip block for block_one
    // and not the genesis block, or else the transactions from the genesis block will be added to
    // the mempool storage's rejection list and tests will fail.
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
        .service(Inbound::new(MAX_INBOUND_CONCURRENCY, setup_rx));
    let inbound_service = BoxService::new(inbound_service);
    let inbound_service = ServiceBuilder::new().buffer(1).service(inbound_service);

    let (misbehavior_sender, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let setup_data = InboundSetupData {
        address_book,
        block_download_peer_set: buffered_peer_set,
        block_verifier,
        mempool: mempool_service.clone(),
        state: state_service.clone(),
        latest_chain_tip,
        misbehavior_sender,
    };
    let r = setup_tx.send(setup_data);
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok(), "unexpected setup channel send failure");

    (
        inbound_service,
        mempool_service,
        committed_blocks,
        added_transactions,
        mock_tx_verifier,
        peer_set,
        state_service,
        chain_tip_change,
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
    // get the last transaction from the Zcash blockchain.
    let last_transaction = network
        .unmined_transactions_in_blocks(..=10)
        .last()
        .unwrap();

    // Insert the last transaction into the mempool storage.
    mempool_service
        .storage()
        .insert(last_transaction.clone(), Vec::new(), None)
        .unwrap();

    vec![last_transaction]
}

/// The semantic block verifier type the [`Inbound`] service is wired with in production.
///
/// The concrete service inside the [`BoxService`] is a
/// [`zebra_consensus::router::BlockVerifierRouter`], so its error type is
/// [`RouterError`] — a bare [`VerifyBlockError`] never escapes this stack, it only
/// ever appears nested inside [`RouterError::Block`].
type SemanticBlockVerifierStub = Buffer<
    BoxService<zebra_consensus::Request, zebra_chain::block::Hash, RouterError>,
    zebra_consensus::Request,
>;

/// Wires up an [`Inbound`] service with a stub semantic block verifier, using the same
/// service types as production, and returns the misbehaviour report receiver.
///
/// Unlike the other tests in this module, the returned receiver is *retained* by the
/// caller, so misbehaviour reports emitted by the gossiped block download cleanup loop
/// can be asserted on. See `GHSA-8hh2-hrf2-cqf4`.
async fn setup_gossiped_block_misbehavior(
    block_verifier: SemanticBlockVerifierStub,
) -> (
    Inbound,
    MockService<Request, Response, PanicAssertion>,
    tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
) {
    let network = Mainnet;
    let state_config = StateConfig::ephemeral();

    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &network,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );
    let address_book = Arc::new(std::sync::Mutex::new(address_book));

    // An empty state, so gossiped blocks are always unknown, and the lookahead limit is
    // measured from the genesis height.
    let (state, _read_only_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::init(state_config, &network, Height::MAX, 0).await;
    let state_service = ServiceBuilder::new().buffer(1).service(state);

    let peer_set = MockService::build()
        .with_max_request_delay(MAX_PEER_SET_REQUEST_DELAY)
        .for_unit_tests();
    let buffered_peer_set = Buffer::new(BoxService::new(peer_set.clone()), 10);

    let mempool_service: MockService<mempool::Request, mempool::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let buffered_mempool = Buffer::new(BoxService::new(mempool_service), 10);

    let (setup_tx, setup_rx) = oneshot::channel();

    // The `Inbound` service is used directly, without a `Buffer` or `load_shed` wrapper,
    // so the test can drive `poll_ready()` — and therefore the download cleanup loop —
    // deterministically.
    let inbound = Inbound::new(MAX_INBOUND_CONCURRENCY, setup_rx);

    let (misbehavior_sender, misbehavior_rx) = tokio::sync::mpsc::channel(10);

    let setup_data = InboundSetupData {
        address_book,
        block_download_peer_set: buffered_peer_set,
        block_verifier,
        mempool: buffered_mempool,
        state: state_service,
        latest_chain_tip,
        misbehavior_sender,
    };
    let r = setup_tx.send(setup_data);
    // We can't expect or unwrap because the returned Result does not implement Debug.
    assert!(r.is_ok(), "unexpected setup channel send failure");

    (inbound, peer_set, misbehavior_rx)
}

/// Repeatedly polls the [`Inbound`] service, so its gossiped block download cleanup loop
/// drains any finished downloads, and returns the first misbehaviour report, if any.
async fn poll_for_misbehavior_report(
    inbound: &mut Inbound,
    misbehavior_rx: &mut tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
) -> Option<(PeerSocketAddr, u32)> {
    for _ in 0..60 {
        let _ = std::future::poll_fn(|cx| inbound.poll_ready(cx)).await;

        if let Ok(report) = misbehavior_rx.try_recv() {
            return Some(report);
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    None
}

/// Sends the gossiped block advertisement, and serves the block to the download task.
async fn advertise_and_serve_block(
    inbound: &mut Inbound,
    peer_set: &mut MockService<Request, Response, PanicAssertion>,
    block: Arc<Block>,
    peer: PeerSocketAddr,
) -> Result<(), BoxError> {
    let hash = block.hash();

    let response = inbound
        .ready()
        .await?
        .call(Request::AdvertiseBlock(hash, Some(peer)))
        .await?;
    assert_eq!(
        response,
        Response::Nil,
        "`AdvertiseBlock` requests should always respond `Ok(Nil)`",
    );

    peer_set
        .expect_request(Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(Response::Blocks(vec![Available((block, Some(peer)))]));

    Ok(())
}

/// A peer that gossips a consensus-invalid block must have its misbehaviour score raised.
///
/// The production inbound block verifier is a `BlockVerifierRouter`, so a failed
/// verification is boxed as a [`RouterError`]. The cleanup loop in `Inbound::poll_ready()`
/// downcast the boxed error to [`VerifyBlockError`] instead, and `Box<dyn Error>::downcast`
/// is an exact `TypeId` match, so the downcast always failed and no peer was ever scored
/// for serving an invalid gossiped block.
///
/// Note that this test *retains* the misbehaviour receiver. Every other test in this module
/// drops it, which is why the bug went unnoticed.
///
/// Regression test for `GHSA-8hh2-hrf2-cqf4`.
#[tokio::test(flavor = "multi_thread")]
async fn gossiped_block_router_error_scores_advertising_peer() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let peer = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));

    // A misbehaviour-scoring consensus failure, wrapped exactly like the router wraps it.
    let block_verifier = Buffer::new(
        BoxService::new(tower::service_fn(|_req: zebra_consensus::Request| async {
            Err::<zebra_chain::block::Hash, RouterError>(RouterError::Block {
                source: Box::new(VerifyBlockError::Subsidy(
                    zebra_chain::parameters::subsidy::SubsidyError::NoCoinbase,
                )),
            })
        })),
        10,
    );

    let (mut inbound, mut peer_set, mut misbehavior_rx) =
        setup_gossiped_block_misbehavior(block_verifier).await;

    advertise_and_serve_block(&mut inbound, &mut peer_set, block, peer).await?;

    let report = poll_for_misbehavior_report(&mut inbound, &mut misbehavior_rx).await;

    assert_eq!(
        report,
        Some((peer, 100)),
        "a peer that gossips a consensus-invalid block must be reported for misbehaviour",
    );

    Ok(())
}

/// A verification failure that is not peer misbehaviour must not raise any score.
///
/// Guards against over-banning after the `GHSA-8hh2-hrf2-cqf4` fix: only errors whose
/// `misbehavior_score()` is non-zero may be reported.
#[tokio::test(flavor = "multi_thread")]
async fn gossiped_block_benign_router_error_does_not_score_advertising_peer() -> Result<(), BoxError>
{
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let peer = PeerSocketAddr::from(([192, 168, 180, 10], 10_000));

    // `VerifyBlockError::ValidateProposal` has a misbehaviour score of zero.
    let block_verifier = Buffer::new(
        BoxService::new(tower::service_fn(|_req: zebra_consensus::Request| async {
            Err::<zebra_chain::block::Hash, RouterError>(RouterError::Block {
                source: Box::new(VerifyBlockError::ValidateProposal(
                    "not peer misbehaviour".into(),
                )),
            })
        })),
        10,
    );

    let (mut inbound, mut peer_set, mut misbehavior_rx) =
        setup_gossiped_block_misbehavior(block_verifier).await;

    advertise_and_serve_block(&mut inbound, &mut peer_set, block, peer).await?;

    let report = poll_for_misbehavior_report(&mut inbound, &mut misbehavior_rx).await;

    assert_eq!(
        report, None,
        "a verification failure with a zero misbehaviour score must not be reported",
    );

    Ok(())
}

/// A block verification timeout must not raise the advertising peer's misbehaviour score.
///
/// The inbound verifier is wrapped in a tower [`tower::timeout::Timeout`], so a slow
/// verification produces a boxed `tower::timeout::error::Elapsed`, not a [`RouterError`].
/// That is a local failure, not evidence that the peer misbehaved.
///
/// Negative control for `GHSA-8hh2-hrf2-cqf4`.
#[tokio::test]
async fn gossiped_block_verify_timeout_does_not_score_advertising_peer() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let peer = PeerSocketAddr::from(([192, 168, 180, 11], 10_000));

    // Signals that the download task has reached the verifier, and is now parked inside
    // the `Timeout` layer.
    let (verifier_called_tx, mut verifier_called_rx) = tokio::sync::mpsc::channel(1);

    let block_verifier = Buffer::new(
        BoxService::new(tower::service_fn(move |_req: zebra_consensus::Request| {
            let verifier_called_tx = verifier_called_tx.clone();
            async move {
                let _ = verifier_called_tx.send(()).await;
                std::future::pending::<Result<zebra_chain::block::Hash, RouterError>>().await
            }
        })),
        10,
    );

    let (mut inbound, mut peer_set, mut misbehavior_rx) =
        setup_gossiped_block_misbehavior(block_verifier).await;

    advertise_and_serve_block(&mut inbound, &mut peer_set, block.clone(), peer).await?;

    verifier_called_rx
        .recv()
        .await
        .expect("the download task should reach the block verifier");

    // Fast-forward past the verification timeout, so tower's `Timeout` layer produces a
    // real `Elapsed` error at exactly the position a `RouterError` would appear.
    tokio::time::pause();
    tokio::time::advance(BLOCK_VERIFY_TIMEOUT + Duration::from_secs(1)).await;
    tokio::time::resume();

    let report = poll_for_misbehavior_report(&mut inbound, &mut misbehavior_rx).await;

    assert_eq!(
        report, None,
        "a block verification timeout must not be reported as peer misbehaviour",
    );

    // Make sure the assertion above isn't vacuous: the timed-out download must actually
    // have been drained by the cleanup loop. If it were still in flight, the per-IP and
    // per-hash caps would drop this second advertisement, and no `BlocksByHash` request
    // would reach the peer set.
    advertise_and_serve_block(&mut inbound, &mut peer_set, block, peer).await?;

    Ok(())
}
