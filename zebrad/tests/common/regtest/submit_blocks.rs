//! Test submitblock RPC method on Regtest.
//!
//! This test will get block templates via the `getblocktemplate` RPC method and submit them as new blocks
//! via the `submitblock` RPC method on Regtest.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use color_eyre::eyre::Result;
use tokio::task::JoinHandle;
use tracing::*;

use zebra_chain::{parameters::Network, serialization::ZcashSerialize};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::methods::get_block_template_rpcs::get_block_template::{
    proposal::TimeSource, proposal_block_from_template, GetBlockTemplate,
};

use crate::common::config::random_known_rpc_port_config;
use zebrad::commands::StartCmd;

/// Number of blocks that should be submitted before the test is considered successful.
const NUM_BLOCKS_TO_SUBMIT: usize = 2_000;

/// - Starts Zebrad
/// - Calls `getblocktemplate` RPC method and submits blocks with empty equihash solutions
/// - Checks that blocks were validated and committed successfully
// TODO: Implement custom serialization for `zebra_network::Config` and spawn a zebrad child process instead?
pub(crate) async fn run(nu5_activation_height: Option<u32>) -> Result<()> {
    let _init_guard = zebra_test::init();
    info!("starting regtest submit_blocks test");

    let network = Network::new_regtest(nu5_activation_height);
    let config = random_known_rpc_port_config(false, &network)?;
    let rpc_address = config.rpc.listen_addr.unwrap();

    info!("starting zebrad");
    let _zebrad_start_task: JoinHandle<_> = tokio::task::spawn_blocking(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime building should not fail");
        let start_cmd = StartCmd::default();

        let _ = rt.block_on(start_cmd.start(Arc::new(config)));
    });

    info!("waiting for zebrad to start");

    tokio::time::sleep(Duration::from_secs(30)).await;

    info!("attempting to submit blocks");
    submit_blocks(rpc_address).await?;

    Ok(())
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
