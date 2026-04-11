use color_eyre::eyre::{Result, WrapErr};

use serde_json::Value;
use zebra_chain::parameters::Network;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::state_database_format_version_in_code;
use zebra_test::prelude::*;

use crate::common::{
    cached_state::{wait_for_state_version_message, wait_for_state_version_upgrade},
    launch::spawn_zebrad_for_rpc,
    sync::SYNC_FINISHED_REGEX,
    test_type::TestType,
};

/// Test successful getblocktemplate rpc call
///
/// See [`crate::common::get_block_template_rpcs::get_block_template`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_get_block_template() -> Result<()> {
    crate::common::get_block_template_rpcs::get_block_template::run().await
}

/// Test successful submitblock rpc call
///
/// See [`crate::common::get_block_template_rpcs::submit_block`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_submit_block() -> Result<()> {
    crate::common::get_block_template_rpcs::submit_block::run().await
}

/// Snapshot the `z_getsubtreesbyindex` method in a synchronized chain.
///
/// This test name must have the same prefix as the `lwd_rpc_test`, so they can be run in the same test job.
#[tokio::test]
#[ignore]
async fn fully_synced_rpc_z_getsubtreesbyindex_snapshot_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network,
        "rpc_z_getsubtreesbyindex_sync_snapshots",
        test_type,
        true,
    )? {
        tracing::info!("running fully synced zebrad z_getsubtreesbyindex RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    // It doesn't matter how long the state version upgrade takes,
    // because the sync finished regex is repeated every minute.
    wait_for_state_version_upgrade(
        &mut zebrad,
        &state_version_message,
        state_database_format_version_in_code(),
        None,
    )?;

    // Wait for zebrad to load the full cached blockchain.
    zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

    // Create an http client
    let client =
        RpcRequestClient::new(zebra_rpc_address.expect("already checked that address is valid"));

    // Create test vector matrix
    let zcashd_test_vectors = vec![
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_1".to_string(),
            r#"["sapling", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_11".to_string(),
            r#"["sapling", 0, 11]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_17_1".to_string(),
            r#"["sapling", 17, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_1090_6".to_string(),
            r#"["sapling", 1090, 6]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_0_1".to_string(),
            r#"["orchard", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_338_1".to_string(),
            r#"["orchard", 338, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_585_1".to_string(),
            r#"["orchard", 585, 1]"#.to_string(),
        ),
    ];

    for i in zcashd_test_vectors {
        let res = client.call("z_getsubtreesbyindex", i.1).await?;
        let body = res.bytes().await;
        let parsed: Value = serde_json::from_slice(&body.expect("Response is valid json"))?;
        insta::assert_json_snapshot!(i.0, parsed);
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
