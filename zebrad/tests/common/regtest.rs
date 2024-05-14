//! Test submitblock RPC method on Regtest.
//!
//! This test will get block templates via the `getblocktemplate` RPC method and submit them as new blocks
//! via the `submitblock` RPC method on Regtest.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use color_eyre::eyre::{Context, Result};
use tracing::*;

use zebra_chain::{
    parameters::{testnet::REGTEST_NU5_ACTIVATION_HEIGHT, Network, NetworkUpgrade},
    primitives::byte_array::increment_big_endian,
    serialization::ZcashSerialize,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::methods::get_block_template_rpcs::get_block_template::{
    proposal::TimeSource, proposal_block_from_template, GetBlockTemplate,
};
use zebra_test::args;

use crate::common::{
    config::{random_known_rpc_port_config, testdir},
    launch::ZebradTestDirExt,
};

/// Number of blocks that should be submitted before the test is considered successful.
const NUM_BLOCKS_TO_SUBMIT: usize = 200;

pub(crate) async fn submit_blocks_test() -> Result<()> {
    let _init_guard = zebra_test::init();
    info!("starting regtest submit_blocks test");

    let network = Network::new_regtest(None);
    let mut config = random_known_rpc_port_config(false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);
    let rpc_address = config.rpc.listen_addr.unwrap();

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("attempting to submit blocks");
    submit_blocks(network, rpc_address).await?;

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}

/// Get block templates and submit blocks
async fn submit_blocks(network: Network, rpc_address: SocketAddr) -> Result<()> {
    let client = RpcRequestClient::new(rpc_address);

    for height in 1..=NUM_BLOCKS_TO_SUBMIT {
        let block_template: GetBlockTemplate = client
            .json_result_from_call("getblocktemplate", "[]".to_string())
            .await
            .expect("response should be success output with a serialized `GetBlockTemplate`");

        let network_upgrade = if height < REGTEST_NU5_ACTIVATION_HEIGHT.try_into().unwrap() {
            NetworkUpgrade::Canopy
        } else {
            NetworkUpgrade::Nu5
        };

        let mut block =
            proposal_block_from_template(&block_template, TimeSource::default(), network_upgrade)?;
        let height = block
            .coinbase_height()
            .expect("should have a coinbase height");

        while !network.disable_pow()
            && zebra_consensus::difficulty_is_valid(&block.header, &network, &height, &block.hash())
                .is_err()
        {
            increment_big_endian(Arc::make_mut(&mut block.header).nonce.as_mut());
        }

        let block_data = hex::encode(block.zcash_serialize_to_vec()?);

        let submit_block_response = client
            .text_from_call("submitblock", format!(r#"["{block_data}"]"#))
            .await?;

        let was_submission_successful = submit_block_response.contains(r#""result":null"#);

        if height.0 % 40 == 0 {
            info!(
                was_submission_successful,
                ?block_template,
                ?network_upgrade,
                "submitted block"
            );
        }

        // Check that the block was validated and committed.
        assert!(
            submit_block_response.contains(r#""result":null"#),
            "unexpected response from submitblock RPC, should be null, was: {submit_block_response}"
        );
    }

    Ok(())
}
