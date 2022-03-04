//! Fixed test vectors for RPC methods.

use std::sync::Arc;

use tower::buffer::Buffer;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
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
    let state = zebra_state::init_test(Network::Mainnet);

    let rpc = RpcImpl::new(
        "Zebra version test".to_string(),
        Buffer::new(mempool.clone(), 1),
        state,
    );

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided string.
    assert_eq!(get_info.build, "Zebra version test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);

    mempool.expect_no_requests().await;
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

    let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
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
            .get_block(Height(i as u32), 0u8)
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block.data, block.into());
    }
}
