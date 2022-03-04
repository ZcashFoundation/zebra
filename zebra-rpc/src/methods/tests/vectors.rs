//! Fixed test vectors for RPC methods.

use std::sync::Arc;

use super::super::*;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use zebra_network::constants::USER_AGENT;

#[tokio::test]
async fn rpc_getinfo() {
    zebra_test::init();

    let state_service = zebra_state::init_test(Network::Mainnet);

    let rpc = RpcImpl {
        app_version: "Zebra version test".to_string(),
        state_service,
    };

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided string.
    assert_eq!(get_info.build, "Zebra version test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);
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

    // Create a populated state service
    let state_service = zebra_state::populated_state(blocks.clone(), Network::Mainnet).await;

    // Init RPC
    let rpc = RpcImpl {
        app_version: "Zebra version test".to_string(),
        state_service,
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
