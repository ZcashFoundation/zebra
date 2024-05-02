//! Test submitblock RPC method on Regtest.
//!
//! This test will get block templates via the `getblocktemplate` RPC method and submit them as new blocks
//! via the `submitblock` RPC method on Regtest.

use std::{net::SocketAddr, time::Duration};

use color_eyre::eyre::{Context, Result};
use tracing::*;

use zebra_chain::{parameters::Network, serialization::ZcashSerialize};
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

    let network = Network::new_regtest();
    let mut config = random_known_rpc_port_config(false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);
    let rpc_address = config.rpc.listen_addr.unwrap();

    let mut zebrad = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("attempting to submit blocks");
    submit_blocks(rpc_address).await?;

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}

/// Get block templates and submit blocks
async fn submit_blocks(rpc_address: SocketAddr) -> Result<()> {
    let client = RpcRequestClient::new(rpc_address);

    for _ in 0..NUM_BLOCKS_TO_SUBMIT {
        let block_template: GetBlockTemplate = client
            .json_result_from_call("getblocktemplate", "[]".to_string())
            .await
            .expect("response should be success output with a serialized `GetBlockTemplate`");

        let block_data = hex::encode(
            proposal_block_from_template(&block_template, TimeSource::default())?
                .zcash_serialize_to_vec()?,
        );

        let submit_block_response = client
            .text_from_call("submitblock", format!(r#"["{block_data}"]"#))
            .await?;

        let was_submission_successful = submit_block_response.contains(r#""result":null"#);

        info!(was_submission_successful, "submitted block");

        // Check that the block was validated and committed.
        assert!(
            submit_block_response.contains(r#""result":null"#),
            "unexpected response from submitblock RPC, should be null, was: {submit_block_response}"
        );
    }

    Ok(())
}
