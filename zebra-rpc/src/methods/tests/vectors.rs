//! Fixed test vectors for RPC methods.

use std::sync::Arc;

use tower::buffer::Buffer;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use super::super::*;

#[tokio::test]
async fn rpc_getinfo() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state = MockService::build().for_unit_tests();

    let rpc = RpcImpl::new(
        "Zebra version test".to_string(),
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
    );

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided string.
    assert_eq!(get_info.build, "Zebra version test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
}

#[tokio::test]
async fn rpc_getblock() {
    zebra_test::init();

    // Number of blocks to populate state with
    const NUMBER_OF_BLOCKS: u32 = 10;

    // Put the first `NUMBER_OF_BLOCKS` blocks in a vector
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=NUMBER_OF_BLOCKS)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let state = zebra_state::populated_state(blocks.clone(), Network::Mainnet).await;

    // Init RPC
    let rpc = RpcImpl {
        app_version: "Zebra version test".to_string(),
        mempool: Buffer::new(mempool.clone(), 1),
        state,
    };

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
    let mut state = MockService::build().for_unit_tests();

    // Init RPC
    let rpc = RpcImpl {
        app_version: "Zebra version test".to_string(),
        mempool: Buffer::new(mempool.clone(), 1),
        state: Buffer::new(state.clone(), 1),
    };

    // Make sure we get an error if Zebra can't parse the block height.
    assert!(rpc
        .get_block("not parsable as height".to_string(), 0u8)
        .await
        .is_err());

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
}
