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
    transaction::{AuthDigest, Hash as TxHash, UnminedTxId, WtxId},
};
use zebra_consensus::{chain::VerifyChainError, error::TransactionError, transaction};
use zebra_network::{
    connect_isolated_tcp_direct, Config as NetworkConfig, InventoryResponse, Request, Response,
    SharedPeerError,
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
    ) = setup().await;

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
    ) = setup().await;

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
    let request = connected_peer_service
        .clone()
        .oneshot(Request::BlocksByHash(iter::once(test_block).collect()));
    let response = request.await;
    match response.as_ref() {
        Err(missing_error) => {
            let missing_error = missing_error
                .downcast_ref::<SharedPeerError>()
                .expect("unexpected inner error type, expected SharedPeerError");
            assert_eq!(
                missing_error.inner_debug(),
                "NotFoundResponse([Block(block::Hash(\"1111111111111111111111111111111111111111111111111111111111111111\"))])"
            );
        }
        _ => unreachable!(
            "peer::Connection should map missing `BlocksByHash` responses as `Err(SharedPeerError(NotFoundResponse(_)))`, \
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

/// Check that a network stack with an empty state responds to single transaction requests with `notfound`.
///
/// Uses a real Zebra network stack, with an isolated Zebra inbound TCP connection.
///
/// TODO: test a response with some Available and some Missing transactions.
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
    ) = setup().await;

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
        let request = connected_peer_service
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()));
        let response = request.await;
        match response.as_ref() {
            Err(missing_error) => {
                let missing_error = missing_error
                    .downcast_ref::<SharedPeerError>()
                    .expect("unexpected inner error type, expected SharedPeerError");

                // Unfortunately, we can't access SharedPeerError's inner type,
                // so we can't compare the actual responses.
                if txs == vec![test_tx] {
                    assert_eq!(
                        missing_error.inner_debug(),
                        "NotFoundResponse([Tx(transaction::Hash(\"2222222222222222222222222222222222222222222222222222222222222222\"))])",
                    );
                } else if txs == vec![test_wtx] {
                    assert_eq!(
                        missing_error.inner_debug(),
                        "NotFoundResponse([Wtx(WtxId { id: transaction::Hash(\"3333333333333333333333333333333333333333333333333333333333333333\"), auth_digest: AuthDigest(\"4444444444444444444444444444444444444444444444444444444444444444\") })])",
                    );
                } else if txs == vec![test_tx, test_wtx] {
                    // The response order is unstable, because it depends on concurrent inbound futures.
                    // In #2244 we will fix this by replacing response Vecs with HashSets.
                    assert!(
                        missing_error.inner_debug() ==
                        "NotFoundResponse([Tx(transaction::Hash(\"2222222222222222222222222222222222222222222222222222222222222222\")), Wtx(WtxId { id: transaction::Hash(\"3333333333333333333333333333333333333333333333333333333333333333\"), auth_digest: AuthDigest(\"4444444444444444444444444444444444444444444444444444444444444444\") })])"
                        ||
                        missing_error.inner_debug() ==
                        "NotFoundResponse([Wtx(WtxId { id: transaction::Hash(\"3333333333333333333333333333333333333333333333333333333333333333\"), auth_digest: AuthDigest(\"4444444444444444444444444444444444444444444444444444444444444444\") }), Tx(transaction::Hash(\"2222222222222222222222222222222222222222222222222222222222222222\"))])",
                        "unexpected response: {:?} \
                         expected response to: {:?}",
                        missing_error.inner_debug(),
                        txs,
                    );
                } else {
                    unreachable!("unexpected test case");
                }
            }
            _ => unreachable!(
                "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundResponse(_)))`, \
                 actual result: {:?}",
                response
            ),
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

/// Check that a network stack returns a `ConnectionReceiveTimeout` error when a peer ignores requests.
/// Then check that repeated requests to a non-responsive peer get a `NotFoundRegistry` error.
///
/// Uses a Zebra network stack's peer set to query an isolated Zebra TCP connection.
/// (Isolated connections have a `Nil` inbound service, so they never respond to requests.)
///
/// The requests are coming from the full stack, so this is the reverse of the previous tests.
#[tokio::test]
async fn outbound_tx_nil_response_notfound() -> Result<(), crate::BoxError> {
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
    ) = setup().await;

    let test_tx = UnminedTxId::from_legacy_id(TxHash([0x55; 32]));
    let test_wtx: UnminedTxId = WtxId {
        id: TxHash([0x66; 32]),
        auth_digest: AuthDigest([0x77; 32]),
    }
    .into();

    // Test both transaction ID variants, separately and together
    for txs in [vec![test_tx], vec![test_wtx], vec![test_tx, test_wtx]] {
        // Make the expected timeout error happen really quickly.
        tokio::time::pause();

        // Send a request via the peer set, via a local TCP connection,
        // to the isolated peer's Nil inbound service
        let response = peer_set
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()))
            .await;
        match response.as_ref() {
            Err(missing_error) => {
                let missing_error = missing_error
                    .downcast_ref::<SharedPeerError>()
                    .expect("unexpected inner error type, expected SharedPeerError");

                // Unfortunately, we can't access SharedPeerError's inner type,
                // so we can't compare the actual responses.
                assert_eq!(
                    missing_error.inner_debug(),
                    "ConnectionReceiveTimeout",
                );
            }
            _ => unreachable!(
                "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(ConnectionReceiveTimeout))`, \
                 actual result: {:?}",
                response
            ),
        };

        // The expected local peer set error should happen immediately.
        tokio::time::resume();

        // Now send the same request to the  peer set,
        // but expect a local failure from the inventory registry.
        let response = peer_set
            .clone()
            .oneshot(Request::TransactionsById(txs.iter().copied().collect()))
            .await;
        match response.as_ref() {
            Err(missing_error) => {
                let missing_error = missing_error
                    .downcast_ref::<SharedPeerError>()
                    .expect("unexpected inner error type, expected SharedPeerError");

                // The only ready peer in the PeerSet has just timed out on the same request,
                // so we expect the peer set to return a `NotFoundRegistry` error immediately.
                //
                // If these asserts fail with ConnectionReceiveTimeout,
                // then the PeerSet isn't using missing inventory to return error responses.
                // (Or the missing inventory from the previous timeout wasn't registered correctly.)
                if txs == vec![test_tx] {
                    assert_eq!(
                        missing_error.inner_debug(),
                        "NotFoundRegistry([Tx(transaction::Hash(\"5555555555555555555555555555555555555555555555555555555555555555\"))])",
                    );
                } else if txs == vec![test_wtx] {
                    assert_eq!(
                        missing_error.inner_debug(),
                        "NotFoundRegistry([Wtx(WtxId { id: transaction::Hash(\"6666666666666666666666666666666666666666666666666666666666666666\"), auth_digest: AuthDigest(\"7777777777777777777777777777777777777777777777777777777777777777\") })])",
                    );
                } else if txs == vec![test_tx, test_wtx] {
                    // The response order is unstable, because it depends on concurrent inbound futures.
                    // In #2244 we will fix this by replacing response Vecs with HashSets.
                    assert!(
                        missing_error.inner_debug() ==
                        "NotFoundRegistry([Tx(transaction::Hash(\"5555555555555555555555555555555555555555555555555555555555555555\")), Wtx(WtxId { id: transaction::Hash(\"6666666666666666666666666666666666666666666666666666666666666666\"), auth_digest: AuthDigest(\"7777777777777777777777777777777777777777777777777777777777777777\") })])"
                        ||
                        missing_error.inner_debug() ==
                        "NotFoundRegistry([Wtx(WtxId { id: transaction::Hash(\"6666666666666666666666666666666666666666666666666666666666666666\"), auth_digest: AuthDigest(\"7777777777777777777777777777777777777777777777777777777777777777\") }), Tx(transaction::Hash(\"5555555555555555555555555555555555555555555555555555555555555555\"))])",
                        "unexpected response: {:?} \
                         expected response to: {:?}",
                        missing_error.inner_debug(),
                        txs,
                    );
                } else {
                    unreachable!("unexpected test case");
                }
            }
            _ => unreachable!(
                "peer::Connection should map missing `TransactionsById` responses as `Err(SharedPeerError(NotFoundRegistry(_)))`, \
                 actual result: {:?}",
                response
            ),
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

/// Setup a real Zebra network stack, with a connected peer using a real isolated network stack.
///
/// Uses fake verifiers, and does not run a block syncer task.
async fn setup() -> (
    // real services
    // connected peer
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

    // Open a fake peer connection to the inbound listener, using the isolated connection API
    let connected_peer_service =
        connect_isolated_tcp_direct(network, listen_addr, "test".to_string())
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
