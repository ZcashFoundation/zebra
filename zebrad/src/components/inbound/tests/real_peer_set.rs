//! Inbound service tests with a real peer set.

use std::{collections::HashSet, iter, net::SocketAddr, sync::Arc};

use futures::FutureExt;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{
    buffer::Buffer,
    builder::ServiceBuilder,
    util::{BoxCloneService, BoxService},
    ServiceExt,
};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{AuthDigest, Hash as TxHash, Transaction, UnminedTx, UnminedTxId, WtxId},
};
use zebra_consensus::{chain::VerifyChainError, error::TransactionError, transaction};
use zebra_network::{
    connect_isolated_tcp_direct_with_inbound, types::InventoryHash, Config as NetworkConfig,
    InventoryResponse, PeerError, Request, Response, SharedPeerError,
};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::{
        inbound::{Inbound, InboundSetupData},
        mempool::{self, gossip_mempool_transaction_id, Mempool},
        sync::{self, BlockGossipError, SyncStatus},
    },
    BoxError,
};

use InventoryResponse::*;

/// Check that a network stack with an empty address book only contains the local listener port,
/// by querying the inbound service via a local TCP connection.
///
/// Uses a real Zebra network stack with a local listener address,
/// and an isolated Zebra inbound TCP connection.
#[tokio::test]
async fn inbound_peers_empty_address_book() -> Result<(), crate::BoxError> {
    let (
        // real services
        connected_peer_service,
        inbound_service,
        _peer_set,
        _mempool_service,
        _state_service,
        // mocked services
        _mock_block_verifier,
        _mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        listen_addr,
    ) = setup(None).await;

    // Send a request to inbound directly
    let request = inbound_service.clone().oneshot(Request::Peers);
    let response = request.await;
    match response.as_ref() {
        Ok(Response::Peers(single_peer)) if single_peer.len() == 1 => {
            assert_eq!(single_peer.first().unwrap().addr(), listen_addr)
        }
        Ok(Response::Peers(_peer_list)) => unreachable!(
            "`Peers` response should contain a single peer, \
             actual result: {:?}",
            response
        ),
        _ => unreachable!(
            "`Peers` requests should always respond `Ok(Response::Peers(_))`, \
             actual result: {:?}",
            response
        ),
    };

    // Send a request via the connected peer, via a local TCP connection, to the inbound service
    let request = connected_peer_service.clone().oneshot(Request::Peers);
    let response = request.await;
    match response.as_ref() {
        Ok(Response::Peers(single_peer)) if single_peer.len() == 1 => {
            assert_eq!(single_peer.first().unwrap().addr(), listen_addr)
        }
        Ok(Response::Peers(_peer_list)) => unreachable!(
            "`Peers` response should contain a single peer, \
             actual result: {:?}",
            response
        ),
        _ => unreachable!(
            "`Peers` requests should always respond `Ok(Response::Peers(_))`, \
             actual result: {:?}",
            response
        ),
    };

    let block_gossip_result = block_gossip_task_handle.now_or_never();
    assert!(
        matches!(block_gossip_result, None),
        "unexpected error or panic in block gossip task: {:?}",
        block_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Check that a network stack with an empty state responds to block requests with `notfound`.
///
/// Uses a real Zebra network stack, with an isolated Zebra inbound TCP connection.
#[tokio::test]
async fn inbound_block_empty_state_notfound() -> Result<(), crate::BoxError> {
    let (
        // real services
        connected_peer_service,
        inbound_service,
        _peer_set,
        _mempool_service,
        _state_service,
        // mocked services
        _mock_block_verifier,
        _mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        _listen_addr,
    ) = setup(None).await;

    let test_block = block::Hash([0x11; 32]);

    // Send a request to inbound directly
    let request = inbound_service
        .clone()
        .oneshot(Request::BlocksByHash(iter::once(test_block).collect()));
    let response = request.await;
    match response.as_ref() {
        Ok(Response::Blocks(single_block)) if single_block.len() == 1 => {
            assert_eq!(single_block.first().unwrap(), &Missing(test_block));
        }
        Ok(Response::Blocks(_block_list)) => unreachable!(
            "`BlocksByHash` response should contain a single block, \
             actual result: {:?}",
            response
        ),
        _ => unreachable!(
            "inbound service should respond to `BlocksByHash` with `Ok(Response::Blocks(_))`, \
             actual result: {:?}",
            response
        ),
    };

    // Send a request via the connected peer, via a local TCP connection, to the inbound service
    let response = connected_peer_service
        .clone()
        .oneshot(Request::BlocksByHash(iter::once(test_block).collect()))
        .await;
    if let Err(missing_error) = response.as_ref() {
        let missing_error = missing_error
            .downcast_ref::<SharedPeerError>()
            .expect("unexpected inner error type, expected SharedPeerError");

        // Unfortunately, we can't access SharedPeerError's inner type,
        // so we can't compare the actual responses.
        let expected = PeerError::NotFoundResponse(vec![InventoryHash::Block(test_block)]);
        let expected = SharedPeerError::from(expected);
        assert_eq!(missing_error.inner_debug(), expected.inner_debug());
    } else {
        unreachable!(
                "peer::Connection should map missing `BlocksByHash` responses as `Err(SharedPeerError(NotFoundResponse(_)))`, \
             actual result: {:?}",
                response
            )
    };

    let block_gossip_result = block_gossip_task_handle.now_or_never();
    assert!(
        matches!(block_gossip_result, None),
        "unexpected error or panic in block gossip task: {:?}",
        block_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Check that a network stack with an empty state responds to single transaction requests with `notfound`.
///
/// Uses a real Zebra network stack, with an isolated Zebra inbound TCP connection.
#[tokio::test]
async fn inbound_tx_empty_state_notfound() -> Result<(), crate::BoxError> {
    let (
        // real services
        connected_peer_service,
        inbound_service,
        _peer_set,
        _mempool_service,
        _state_service,
        // mocked services
        _mock_block_verifier,
        _mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        _listen_addr,
    ) = setup(None).await;

    let test_tx = UnminedTxId::from_legacy_id(TxHash([0x22; 32]));
    let test_wtx: UnminedTxId = WtxId {
        id: TxHash([0x33; 32]),
        auth_digest: AuthDigest([0x44; 32]),
    }
    .into();

    // Test both transaction ID variants, separately and together
    for txs in [vec![test_tx], vec![test_wtx], vec![test_tx, test_wtx]] {
        // Send a request to inbound directly
        let request = inbound_service
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()));
        let response = request.await;
        match response.as_ref() {
            Ok(Response::Transactions(response_txs)) => {
                // The response order is unstable, because it depends on concurrent inbound futures.
                // In #2244 we will fix this by replacing response Vecs with HashSets.
                for tx in &txs {
                    assert!(
                        response_txs.contains(&Missing(*tx)),
                        "expected {:?}, but it was not in the response",
                        tx
                    );
                }
                assert_eq!(response_txs.len(), txs.len());
            }
            _ => unreachable!(
                "inbound service should respond to `TransactionsById` with `Ok(Response::Transactions(_))`, \
                 actual result: {:?}",
                response
            ),
        };

        // Send a request via the connected peer, via a local TCP connection, to the inbound service
        let response = connected_peer_service
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()))
            .await;
        if let Err(missing_error) = response.as_ref() {
            let missing_error = missing_error
                .downcast_ref::<SharedPeerError>()
                .expect("unexpected inner error type, expected SharedPeerError");

            // Unfortunately, we can't access SharedPeerError's inner type,
            // so we can't compare the actual responses.
            if txs.len() <= 1 {
                let expected = PeerError::NotFoundResponse(
                    txs.iter().copied().map(InventoryHash::from).collect(),
                );
                let expected = SharedPeerError::from(expected);
                assert_eq!(missing_error.inner_debug(), expected.inner_debug());
            } else {
                // The response order is unstable, because it depends on concurrent inbound futures.
                // In #2244 we will fix this by replacing response Vecs with HashSets.
                //
                // Assume there are only 2 transactions.
                let expected1: Vec<InventoryHash> =
                    txs.iter().copied().map(InventoryHash::from).collect();
                let expected2: Vec<InventoryHash> =
                    txs.iter().rev().copied().map(InventoryHash::from).collect();

                let expected: Vec<String> = [expected1, expected2]
                    .into_iter()
                    .map(PeerError::NotFoundResponse)
                    .map(|error| SharedPeerError::from(error).inner_debug())
                    .collect();
                let actual = missing_error.inner_debug();

                assert!(
                    expected.iter().any(|expected| expected == &actual),
                    "unexpected response: {:?} \
                     expected one of: {:?}",
                    actual,
                    expected,
                );
            }
        } else {
            unreachable!(
                        "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundResponse(_)))`, \
                 actual result: {:?}",
                        response
                    )
        };
    }

    let block_gossip_result = block_gossip_task_handle.now_or_never();
    assert!(
        matches!(block_gossip_result, None),
        "unexpected error or panic in block gossip task: {:?}",
        block_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Check that a network stack:
/// - returns a `NotFound` error when a peer responds with an unrelated transaction, and
/// - returns a `NotFoundRegistry` error for repeated requests to a non-responsive peer.
///
/// The requests are coming from the full stack to the isolated peer,
/// so this is the reverse of the previous tests.
///
/// Uses a Zebra network stack's peer set to query an isolated Zebra TCP connection,
/// with an unrelated transaction test responder.
#[tokio::test]
async fn outbound_tx_unrelated_response_notfound() -> Result<(), crate::BoxError> {
    // We respond with an unrelated transaction, so the peer gives up on the request.
    let unrelated_response: Transaction =
        zebra_test::vectors::DUMMY_TX1.zcash_deserialize_into()?;
    let unrelated_response = Response::Transactions(vec![Available(unrelated_response.into())]);

    let (
        // real services
        _connected_peer_service,
        _inbound_service,
        peer_set,
        _mempool_service,
        _state_service,
        // mocked services
        _mock_block_verifier,
        _mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        _listen_addr,
    ) = setup(Some(unrelated_response)).await;

    let test_tx5 = UnminedTxId::from_legacy_id(TxHash([0x55; 32]));
    let test_wtx67: UnminedTxId = WtxId {
        id: TxHash([0x66; 32]),
        auth_digest: AuthDigest([0x77; 32]),
    }
    .into();
    let test_tx8 = UnminedTxId::from_legacy_id(TxHash([0x88; 32]));
    let test_wtx91: UnminedTxId = WtxId {
        id: TxHash([0x99; 32]),
        auth_digest: AuthDigest([0x11; 32]),
    }
    .into();

    // Test both transaction ID variants, separately and together.
    // These IDs all need to be different, to avoid polluting the inventory registry between tests.
    for txs in [vec![test_tx5], vec![test_wtx67], vec![test_tx8, test_wtx91]] {
        // Send a request via the peer set, via a local TCP connection,
        // to the isolated peer's `unrelated_response` inbound service
        let response = peer_set
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()))
            .await;

        // Unfortunately, we can't access SharedPeerError's inner type,
        // so we can't compare the actual responses.
        if let Err(missing_error) = response.as_ref() {
            let missing_error = missing_error
                .downcast_ref::<SharedPeerError>()
                .expect("unexpected inner error type, expected SharedPeerError");

            if txs.len() <= 1 {
                let expected = PeerError::NotFoundResponse(
                    txs.iter().copied().map(InventoryHash::from).collect(),
                );
                let expected = SharedPeerError::from(expected);
                assert_eq!(missing_error.inner_debug(), expected.inner_debug());
            } else {
                // The response order is unstable, because it depends on concurrent inbound futures.
                // In #2244 we will fix this by replacing response Vecs with HashSets.
                //
                // Assume there are only 2 transactions.
                let expected1: Vec<InventoryHash> =
                    txs.iter().copied().map(InventoryHash::from).collect();
                let expected2: Vec<InventoryHash> =
                    txs.iter().rev().copied().map(InventoryHash::from).collect();

                let expected: Vec<String> = [expected1, expected2]
                    .into_iter()
                    .map(PeerError::NotFoundResponse)
                    .map(|error| SharedPeerError::from(error).inner_debug())
                    .collect();
                let actual = missing_error.inner_debug();

                assert!(
                    expected.iter().any(|expected| expected == &actual),
                    "unexpected response: {:?} \
                     expected one of: {:?}",
                    actual,
                    expected,
                );
            }
        } else {
            unreachable!(
                        "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundResponse(_)))`, \
                 actual result: {:?}",
                        response
                    )
        };

        // The peer set only does routing for single-transaction requests.
        // (But the inventory tracker tracks the response to requests of any size.)
        for tx in &txs {
            // Now send the same request to the  peer set,
            // but expect a local failure from the inventory registry.
            let response = peer_set
                .clone()
                .oneshot(Request::TransactionsById(iter::once(tx).copied().collect()))
                .await;

            // The only ready peer in the PeerSet failed the same request,
            // so we expect the peer set to return a `NotFoundRegistry` error immediately.
            //
            // If these asserts fail, then the PeerSet isn't returning inv routing error responses.
            // (Or the missing inventory from the previous timeout wasn't registered correctly.)
            if let Err(missing_error) = response.as_ref() {
                let missing_error = missing_error
                    .downcast_ref::<SharedPeerError>()
                    .expect("unexpected inner error type, expected SharedPeerError");

                // Unfortunately, we can't access SharedPeerError's inner type,
                // so we can't compare the actual responses.
                let expected = PeerError::NotFoundRegistry(vec![InventoryHash::from(*tx)]);
                let expected = SharedPeerError::from(expected);
                assert_eq!(missing_error.inner_debug(), expected.inner_debug());
            } else {
                unreachable!(
                    "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundRegistry(_)))`, \
                     actual result: {:?}",
                    response
                )
            };
        }
    }

    let block_gossip_result = block_gossip_task_handle.now_or_never();
    assert!(
        matches!(block_gossip_result, None),
        "unexpected error or panic in block gossip task: {:?}",
        block_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Check that a network stack:
/// - returns a partial notfound response, when a peer partially responds to a multi-transaction request,
/// - returns a `NotFoundRegistry` error for repeated requests to a non-responsive peer.
///
/// The requests are coming from the full stack to the isolated peer.
#[tokio::test]
async fn outbound_tx_partial_response_notfound() -> Result<(), crate::BoxError> {
    // We repeatedly respond with the same transaction, so the peer gives up on the second response.
    let repeated_tx: Transaction = zebra_test::vectors::DUMMY_TX1.zcash_deserialize_into()?;
    let repeated_tx: UnminedTx = repeated_tx.into();
    let repeated_response = Response::Transactions(vec![
        Available(repeated_tx.clone()),
        Available(repeated_tx.clone()),
    ]);

    let (
        // real services
        _connected_peer_service,
        _inbound_service,
        peer_set,
        _mempool_service,
        _state_service,
        // mocked services
        _mock_block_verifier,
        _mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        _listen_addr,
    ) = setup(Some(repeated_response)).await;

    let missing_tx_id = UnminedTxId::from_legacy_id(TxHash([0x22; 32]));

    let txs = [missing_tx_id, repeated_tx.id];

    // Send a request via the peer set, via a local TCP connection,
    // to the isolated peer's `repeated_response` inbound service
    let response = peer_set
        .clone()
        .oneshot(Request::TransactionsById(txs.iter().copied().collect()))
        .await;

    if let Ok(Response::Transactions(tx_response)) = response.as_ref() {
        let available: Vec<UnminedTx> = tx_response
            .iter()
            .filter_map(InventoryResponse::available)
            .collect();
        let missing: Vec<UnminedTxId> = tx_response
            .iter()
            .filter_map(InventoryResponse::missing)
            .collect();

        assert_eq!(available, vec![repeated_tx]);
        assert_eq!(missing, vec![missing_tx_id]);
    } else {
        unreachable!(
            "peer::Connection should map partial `TransactionsById` responses as `Ok(Response::Transactions(_))`, \
             actual result: {:?}",
            response
        )
    };

    // Now send the same request to the  peer set,
    // but expect a local failure from the inventory registry.
    //
    // The peer set only does routing for single-transaction requests.
    // (But the inventory tracker tracks the response to requests of any size.)
    let response = peer_set
        .clone()
        .oneshot(Request::TransactionsById(
            iter::once(missing_tx_id).collect(),
        ))
        .await;

    // The only ready peer in the PeerSet failed the same request,
    // so we expect the peer set to return a `NotFoundRegistry` error immediately.
    //
    // If these asserts fail, then the PeerSet isn't returning inv routing error responses.
    // (Or the missing inventory from the previous timeout wasn't registered correctly.)
    if let Err(missing_error) = response.as_ref() {
        let missing_error = missing_error
            .downcast_ref::<SharedPeerError>()
            .expect("unexpected inner error type, expected SharedPeerError");

        // Unfortunately, we can't access SharedPeerError's inner type,
        // so we can't compare the actual responses.
        let expected = PeerError::NotFoundRegistry(vec![InventoryHash::from(missing_tx_id)]);
        let expected = SharedPeerError::from(expected);
        assert_eq!(missing_error.inner_debug(), expected.inner_debug());
    } else {
        unreachable!(
            "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundRegistry(_)))`, \
             actual result: {:?}",
            response
        )
    };

    let block_gossip_result = block_gossip_task_handle.now_or_never();
    assert!(
        matches!(block_gossip_result, None),
        "unexpected error or panic in block gossip task: {:?}",
        block_gossip_result,
    );

    let tx_gossip_result = tx_gossip_task_handle.now_or_never();
    assert!(
        matches!(tx_gossip_result, None),
        "unexpected error or panic in transaction gossip task: {:?}",
        tx_gossip_result,
    );

    Ok(())
}

/// Setup a real Zebra network stack, with a connected peer using a real isolated network stack.
///
/// The isolated peer responds to every request with `isolated_peer_response`.
/// (If no response is provided, the isolated peer ignores inbound requests.)
///
/// Uses fake verifiers, and does not run a block syncer task.
async fn setup(
    isolated_peer_response: Option<Response>,
) -> (
    // real services
    // connected peer which responds with isolated_peer_response
    Buffer<
        BoxService<zebra_network::Request, zebra_network::Response, BoxError>,
        zebra_network::Request,
    >,
    // inbound service
    BoxCloneService<zebra_network::Request, zebra_network::Response, BoxError>,
    // outbound peer set (only has the connected peer)
    Buffer<
        BoxService<zebra_network::Request, zebra_network::Response, BoxError>,
        zebra_network::Request,
    >,
    Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>,
    Buffer<BoxService<zebra_state::Request, zebra_state::Response, BoxError>, zebra_state::Request>,
    // mocked services
    MockService<Arc<Block>, block::Hash, PanicAssertion, VerifyChainError>,
    MockService<transaction::Request, transaction::Response, PanicAssertion, TransactionError>,
    // real tasks
    JoinHandle<Result<(), BlockGossipError>>,
    JoinHandle<Result<(), BoxError>>,
    // real open socket addresses
    SocketAddr,
) {
    zebra_test::init();

    let network = Network::Mainnet;
    // Open a listener on an unused IPv4 localhost port
    let config_listen_addr = "127.0.0.1:0".parse().unwrap();

    // Inbound
    let (setup_tx, setup_rx) = oneshot::channel();
    let inbound_service = Inbound::new(setup_rx);
    let inbound_service = ServiceBuilder::new()
        .boxed_clone()
        .load_shed()
        .buffer(10)
        .service(inbound_service);

    // State
    let state_config = StateConfig::ephemeral();
    let (state_service, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(10).service(state_service);

    // Network
    let network_config = NetworkConfig {
        network,
        listen_addr: config_listen_addr,

        // Stop Zebra making outbound connections
        initial_mainnet_peers: HashSet::new(),
        initial_testnet_peers: HashSet::new(),

        ..NetworkConfig::default()
    };
    let (mut peer_set, address_book) = zebra_network::init(
        network_config,
        inbound_service.clone(),
        latest_chain_tip.clone(),
    )
    .await;

    // Inbound listener
    let listen_addr = address_book
        .lock()
        .unwrap()
        .local_listener_meta_addr()
        .addr();
    assert_ne!(
        listen_addr.port(),
        0,
        "dynamic ports are replaced with OS-assigned ports"
    );
    assert_eq!(
        listen_addr.ip(),
        config_listen_addr.ip(),
        "IP addresses are correctly propagated"
    );

    // Fake syncer
    let (sync_status, mut recent_syncs) = SyncStatus::new();

    // Fake verifiers
    let mock_block_verifier = MockService::build().for_unit_tests();
    let buffered_block_verifier = ServiceBuilder::new()
        .buffer(10)
        .service(BoxService::new(mock_block_verifier.clone()));
    let mock_tx_verifier = MockService::build().for_unit_tests();
    let buffered_tx_verifier = ServiceBuilder::new()
        .buffer(10)
        .service(BoxService::new(mock_tx_verifier.clone()));

    // Mempool
    let mempool_config = mempool::Config::default();
    let (mut mempool_service, transaction_receiver) = Mempool::new(
        &mempool_config,
        peer_set.clone(),
        state_service.clone(),
        buffered_tx_verifier.clone(),
        sync_status.clone(),
        latest_chain_tip.clone(),
        chain_tip_change.clone(),
    );

    // Enable the mempool
    mempool_service.enable(&mut recent_syncs).await;
    let mempool_service = ServiceBuilder::new()
        .buffer(10)
        .boxed()
        // boxed() needs this extra tiny buffer
        .buffer(1)
        .service(mempool_service);

    // Initialize the inbound service
    let setup_data = InboundSetupData {
        address_book,
        block_download_peer_set: peer_set.clone(),
        block_verifier: buffered_block_verifier,
        mempool: mempool_service.clone(),
        state: state_service.clone(),
        latest_chain_tip,
    };
    let r = setup_tx.send(setup_data);
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok(), "unexpected setup channel send failure");

    let block_gossip_task_handle = tokio::spawn(sync::gossip_best_tip_block_hashes(
        sync_status.clone(),
        chain_tip_change,
        peer_set.clone(),
    ));

    let tx_gossip_task_handle = tokio::spawn(gossip_mempool_transaction_id(
        transaction_receiver,
        peer_set.clone(),
    ));

    // Set up the inbound service response for the isolated peer
    let isolated_peer_response = isolated_peer_response.unwrap_or(Response::Nil);
    let response_inbound_service = tower::service_fn(move |_req| {
        let isolated_peer_response = isolated_peer_response.clone();
        async move { Ok::<Response, BoxError>(isolated_peer_response) }
    });
    let user_agent = "test".to_string();

    // Open a fake peer connection to the inbound listener, using the isolated connection API
    let connected_peer_service = connect_isolated_tcp_direct_with_inbound(
        network,
        listen_addr,
        user_agent,
        response_inbound_service,
    )
    .await
    .expect("local listener connection succeeds");
    let connected_peer_service = ServiceBuilder::new()
        .buffer(10)
        .service(connected_peer_service);

    // Make the peer set find the new peer
    let _ = peer_set
        .ready()
        .await
        .expect("peer set becomes ready without errors");

    // there is no syncer task, and the verifiers are fake,
    // but the network stack is all real
    (
        // real services
        connected_peer_service,
        inbound_service,
        peer_set,
        mempool_service,
        state_service,
        // mocked services
        mock_block_verifier,
        mock_tx_verifier,
        // real tasks
        block_gossip_task_handle,
        tx_gossip_task_handle,
        // real open socket addresses
        listen_addr,
    )
}
