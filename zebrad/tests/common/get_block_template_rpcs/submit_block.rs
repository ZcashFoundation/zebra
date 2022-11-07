//! Test submitblock RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will get the first 20 blocks in the non-finalized state
//! (past the MAX_BLOCK_REORG_HEIGHT) via getblock rpc calls, get the finalized tip height
//! of the updated cached state, restart zebra without peers, and submit blocks above the
//! finalized tip height.

use color_eyre::eyre::{Context, Result};

use reqwest::Client;
use zebra_chain::parameters::Network;

use crate::common::{
    launch::{can_spawn_zebrad_for_rpc, spawn_zebrad_for_rpc},
    lightwalletd::LightwalletdTestType,
};

#[allow(clippy::print_stderr)]
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = LightwalletdTestType::UpdateZebraCachedState;
    let test_name = "submit_block_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it
    if !can_spawn_zebrad_for_rpc(test_name, test_type) {
        return Ok(());
    }

    let zebrad_state_path = test_type.zebrad_state_path(test_name);
    let zebrad_state_path = match zebrad_state_path {
        Some(zebrad_state_path) => zebrad_state_path,
        None => return Ok(()),
    };

    tracing::info!(
        ?network,
        ?test_type,
        ?zebrad_state_path,
        "running submitblock test using zebrad",
    );

    // TODO: As part of or as a pre-cursor to issue #5015,
    // - Use only original cached state,
    // - sync until the tip
    // - get first X blocks in non-finalized state via getblock rpc calls
    // - restart zebra without peers
    // - submit block(s) above the finalized tip height
    let raw_blocks: Vec<String> = get_raw_future_blocks(network, zebrad_state_path.clone()).await?;

    tracing::info!(
        partial_sync_path = ?zebrad_state_path,
        "got blocks to submit, spawning isolated zebrad...",
    );

    // Start zebrad with no peers, we run the rest of the submitblock test without syncing.
    let should_sync = false;
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
    {
        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Submitblock test

    let rpc_address = zebra_rpc_address.expect("submitblock test must have RPC port");

    tracing::info!(
        ?test_type,
        ?rpc_address,
        "spawned isolated zebrad with shorter chain, waiting for zebrad to open its RPC port..."
    );
    zebrad.expect_stdout_line_matches(&format!("Opened RPC endpoint at {rpc_address}"))?;

    tracing::info!(?rpc_address, "zebrad opened its RPC port",);

    // Create an http client
    let client = Client::new();

    for raw_block in raw_blocks {
        let res = client
        .post(format!("http://{}", &rpc_address))
        .body(format!(
            r#"{{"jsonrpc": "2.0", "method": "submitblock", "params": ["{raw_block}"], "id":123 }}"#
        ))
        .header("Content-Type", "application/json")
        .send()
        .await?;

        assert!(res.status().is_success());
        let res_text = res.text().await?;

        // Test rpc endpoint response
        assert!(res_text.contains(r#""result":"null""#));
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

/// Accepts a network and a path to a cached zebra state.
///
/// Syncs zebra until the tip, gets some blocks near the tip, via getblock rpc calls,
/// shuts down zebra, and gets the finalized tip height of the updated cached state.
///
/// Returns retrieved blocks that are above the finalized tip height of the cached state.
async fn get_raw_future_blocks(
    network: Network,
    zebra_state_path: std::path::PathBuf,
) -> Result<Vec<String>> {
    todo!()
}
