//! Fixed test vectors for RPC methods.

use std::sync::Arc;

use tower::buffer::Buffer;

use zebra_chain::{
    block::Block, chain_tip::NoChainTip, parameters::Network::*,
    serialization::ZcashDeserializeInto,
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::BoxError;

use zebra_test::mock_service::MockService;

use super::super::*;

#[tokio::test]
async fn rpc_getinfo() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided string.
    assert_eq!(get_info.build, "RPC test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
}

#[tokio::test]
async fn rpc_getblock() {
    zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Make calls and check response
    for (i, block) in blocks.into_iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), 0u8)
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block.0, block.into());
    }

    mempool.expect_no_requests().await;
}

#[tokio::test]
async fn rpc_getblock_error() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    // Init RPC
    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    // Make sure we get an error if Zebra can't parse the block height.
    assert!(rpc
        .get_block("not parsable as height".to_string(), 0u8)
        .await
        .is_err());

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
}

#[tokio::test]
async fn rpc_getbestblockhash() {
    zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Get the hash of the block at the tip using hardcoded block tip bytes.
    // We want to test the RPC response is equal to this hash
    let tip_block = blocks.last().unwrap();
    let tip_block_hash = tip_block.hash();

    // Get a mempool handle
    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service, the tip will be in `NUMBER_OF_BLOCKS`.
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Get the tip hash using RPC method `get_best_block_hash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBestBlockHash struct");
    let response_hash = get_best_block_hash.0;

    // Check if response is equal to block 10 hash.
    assert_eq!(response_hash, tip_block_hash);

    mempool.expect_no_requests().await;
}
