//! Test vectors for raw block, raw transaction, and header-only treestate RPCs.

use std::sync::Arc;

use tower::buffer::Buffer;

use zebra_chain::{
    block::Block,
    chain_sync_status::MockSyncStatus,
    chain_tip::NoChainTip,
    parameters::Network::Mainnet,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use super::super::*;

/// Verbosity-0 `getblock` and `getrawtransaction` return byte-identical serialized payloads
/// when served from stored bytes.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblock_and_getrawtransaction_verbosity_0_match_serialized_bytes() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mempool = tower::service_fn(|_req: mempool::Request| async {
        Ok::<_, BoxError>(mempool::Response::Transactions(vec![]))
    });
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks.clone(), &Mainnet).await;
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, rpc_tx_queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        Buffer::new(mempool, 1),
        Buffer::new(state, 1),
        Buffer::new(read_state, 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    for block in blocks {
        let height = block.coinbase_height().unwrap();
        let expected_block_bytes = block.zcash_serialize_to_vec().unwrap();
        assert_eq!(
            rpc.get_block(height.0.to_string(), Some(0))
                .await
                .expect("block exists"),
            GetBlockResponse::Raw(expected_block_bytes.clone().into())
        );

        for tx in &block.transactions {
            let expected_tx_bytes = tx.zcash_serialize_to_vec().unwrap();
            for block_hash in [None, Some(block.hash().to_string())] {
                assert_eq!(
                    rpc.get_raw_transaction(tx.hash().to_string(), Some(0), block_hash)
                        .await
                        .expect("transaction exists"),
                    GetRawTransactionResponse::Raw(expected_tx_bytes.clone().into())
                );
            }
        }
    }

    rpc_tx_queue.abort();
}

/// `z_gettreestate` reports a missing block, not empty trees, when an active pool's tree lookup
/// finds nothing after the header lookup succeeded.
#[tokio::test(flavor = "multi_thread")]
async fn z_get_treestate_reports_missing_block_when_tree_vanishes() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();

    for missing_orchard in [false, true] {
        // Sapling is active at both heights; Orchard only at the NU5 one.
        let height = if missing_orchard {
            zebra_chain::parameters::NetworkUpgrade::Nu5
        } else {
            zebra_chain::parameters::NetworkUpgrade::Sapling
        }
        .activation_height(&Mainnet)
        .unwrap();

        let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut read_state: MockService<_, _, _, BoxError> = MockService::build()
            .with_max_request_delay(std::time::Duration::from_secs(5))
            .for_unit_tests();
        let (_tx, rx) = tokio::sync::watch::channel(None);
        let (rpc, _rpc_tx_queue) = RpcImpl::new(
            Mainnet,
            Default::default(),
            Default::default(),
            "0.0.1",
            "RPC test",
            Buffer::new(mempool, 1),
            Buffer::new(state, 1),
            Buffer::new(read_state.clone(), 1),
            MockService::build().for_unit_tests(),
            MockSyncStatus::default(),
            NoChainTip,
            MockAddressBookPeers::default(),
            rx,
            None,
        );

        let treestate = tokio::spawn(async move { rpc.z_get_treestate(hash.to_string()).await });

        read_state
            .expect_request(ReadRequest::BlockHeader(hash.into()))
            .await
            .respond(ReadResponse::BlockHeader {
                header: block.header.clone(),
                hash,
                height,
                next_block_hash: None,
            });
        let sapling = read_state
            .expect_request(ReadRequest::AnyChainSaplingTree(hash))
            .await;
        if missing_orchard {
            sapling.respond(ReadResponse::SaplingTree(Some(Default::default())));
            read_state
                .expect_request(ReadRequest::AnyChainOrchardTree(hash))
                .await
                .respond(ReadResponse::OrchardTree(None));
        } else {
            sapling.respond(ReadResponse::SaplingTree(None));
        }

        let error = treestate
            .await
            .expect("task should not panic")
            .expect_err("a vanished tree should be an error");
        assert_eq!(error.code(), -8, "missing_orchard: {missing_orchard}");
        assert_eq!(
            error.message(),
            "the requested block is not in the main chain"
        );
    }
}

/// `z_gettreestate` maps missing-header errors to `-8` while preserving operational and
/// database corruption errors as `-1` (`LegacyCode::Misc`).
#[tokio::test(flavor = "multi_thread")]
async fn z_get_treestate_preserves_operational_errors() {
    let _init_guard = zebra_test::init();

    let hash = zebra_chain::block::Hash([0x11; 32]);
    for (state_err, expected_code, expected_message) in [
        (
            BoxError::from("block hash or height not found"),
            -8,
            "the requested block is not in the main chain",
        ),
        (
            BoxError::from(zebra_state::BlockReadError::CorruptBlock {
                height: zebra_chain::block::Height(42),
            }),
            -1,
            "corrupt block metadata at height Height(42)",
        ),
        (
            BoxError::from("unexpected read failure"),
            -1,
            "unexpected read failure",
        ),
    ] {
        let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut read_state: MockService<_, _, _, BoxError> = MockService::build()
            .with_max_request_delay(std::time::Duration::from_secs(5))
            .for_unit_tests();
        let (_tx, rx) = tokio::sync::watch::channel(None);
        let (rpc, _rpc_tx_queue) = RpcImpl::new(
            Mainnet,
            Default::default(),
            Default::default(),
            "0.0.1",
            "RPC test",
            Buffer::new(mempool, 1),
            Buffer::new(state, 1),
            Buffer::new(read_state.clone(), 1),
            MockService::build().for_unit_tests(),
            MockSyncStatus::default(),
            NoChainTip,
            MockAddressBookPeers::default(),
            rx,
            None,
        );

        let treestate = tokio::spawn(async move { rpc.z_get_treestate(hash.to_string()).await });

        read_state
            .expect_request(ReadRequest::BlockHeader(hash.into()))
            .await
            .respond(Err(state_err));

        let error = treestate
            .await
            .expect("task should not panic")
            .expect_err("header read error should fail");
        assert_eq!(error.code(), expected_code);
        assert_eq!(error.message(), expected_message);
    }
}
