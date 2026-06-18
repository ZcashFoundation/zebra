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

/// Make a synced `getblock` RPC call against cached Zebra state.
#[tokio::test]
#[ignore]
async fn rpc_get_block_from_cached_state() -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_type = TestType::UpdateCachedState;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, "rpc_get_block_from_cached_state", test_type, false)?
    {
        tracing::info!("running fully synced zebrad RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("cached-state RPC test must have RPC port");

    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    let client = RpcRequestClient::new(zebra_rpc_address);

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let res = client
        .text_from_call("getblock", r#"["1180900", 0]"#.to_string())
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    let expected_bytes = zebra_test::vectors::MAINNET_BLOCKS
        .get(&1_180_900)
        .expect("test block must exist");
    let expected_hex = hex::encode(expected_bytes);
    assert!(
        res.contains(&expected_hex),
        "response did not contain the desired block: {res}"
    );

    Ok(())
}

/// Snapshot the `z_getsubtreesbyindex` method in a synchronized chain.
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
        let body = res
            .bytes()
            .await
            .wrap_err("failed to read z_getsubtreesbyindex response body")?;
        let parsed: Value =
            serde_json::from_slice(&body).wrap_err("invalid z_getsubtreesbyindex JSON response")?;
        insta::assert_json_snapshot!(i.0, parsed);
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}
