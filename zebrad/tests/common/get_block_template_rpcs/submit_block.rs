//! Test submitblock RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will get the first 20 blocks in the non-finalized state
//! (past the MAX_BLOCK_REORG_HEIGHT) via getblock rpc calls, get the finalized tip height
//! of the updated cached state, restart zebra without peers, and submit blocks above the
//! finalized tip height.

use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};

use reqwest::Client;
use zebra_chain::{block::Height, parameters::Network};
use zebra_state::MAX_BLOCK_REORG_HEIGHT;

use crate::common::{
    cached_state::load_tip_height_from_state_directory,
    launch::{can_spawn_zebrad_for_rpc, spawn_zebrad_for_rpc},
    lightwalletd::LightwalletdTestType,
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
};

#[allow(clippy::print_stderr)]
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = LightwalletdTestType::UpdateZebraCachedState;
    let test_name = "submit_block_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_rpc(test_name, test_type) {
        return Ok(());
    }

    tracing::info!(
        ?network,
        ?test_type,
        "running submitblock test using zebrad",
    );

    let raw_blocks: Vec<String> = get_raw_future_blocks(network, test_type, test_name, 3).await?;

    tracing::info!("got raw future blocks, spawning isolated zebrad...",);

    // Start zebrad with no peers, we run the rest of the submitblock test without syncing.
    let should_sync = false;
    let (mut zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
            .expect("Already checked zebra state path with can_spawn_zebrad_for_rpc");

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
        assert!(res_text.contains(r#""result":null"#));
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

/// Accepts a network, test_type, test_name, and num_blocks (how many blocks past the finalized tip to try getting)
///
/// Syncs zebra until the tip, gets some blocks near the tip, via getblock rpc calls,
/// shuts down zebra, and gets the finalized tip height of the updated cached state.
///
/// Returns retrieved blocks that are above the finalized tip height of the cached state.
async fn get_raw_future_blocks(
    network: Network,
    test_type: LightwalletdTestType,
    test_name: &str,
    max_num_blocks: u32,
) -> Result<Vec<String>> {
    let max_num_blocks = max_num_blocks.min(MAX_BLOCK_REORG_HEIGHT);
    let mut raw_blocks = Vec::with_capacity(max_num_blocks as usize);

    assert!(
        test_type.needs_zebra_cached_state() && test_type.needs_zebra_rpc_server(),
        "get_raw_future_blocks needs zebra cached state and rpc server"
    );

    let should_sync = true;
    let (zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
            .ok_or_else(|| eyre!("get_raw_future_blocks requires a cached state"))?;
    let rpc_address = zebra_rpc_address.expect("test type must have RPC port");

    let mut zebrad = check_sync_logs_until(
        zebrad,
        network,
        SYNC_FINISHED_REGEX,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
    )?;

    // Create an http client
    let client = Client::new();

    let send_rpc_request = |method, params| {
        client
            .post(format!("http://{}", &rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":123 }}"#
            ))
            .header("Content-Type", "application/json")
            .send()
    };

    let blockchain_info: serde_json::Value = serde_json::from_str(
        &send_rpc_request("getblockchaininfo", "[]".to_string())
            .await?
            .text()
            .await?,
    )?;

    let tip_height: u32 = blockchain_info["result"]["blocks"]
        .as_u64()
        .expect("unexpected block height: doesn't fit in u64")
        .try_into()
        .expect("unexpected block height: doesn't fit in u32");

    let estimated_finalized_tip_height = tip_height - MAX_BLOCK_REORG_HEIGHT;

    tracing::info!(
        ?estimated_finalized_tip_height,
        "got tip height from blockchaininfo",
    );

    for block_height in (0..max_num_blocks).map(|idx| idx + estimated_finalized_tip_height) {
        let raw_block: serde_json::Value = serde_json::from_str(
            &send_rpc_request("getblock", format!(r#"["{block_height}", 0]"#))
                .await?
                .text()
                .await?,
        )?;

        raw_blocks.push((
            block_height,
            raw_block["result"]
                .as_str()
                .expect("unexpected getblock result: not a string")
                .to_string(),
        ));
    }

    zebrad.kill(true)?;

    // Sleep for a few seconds to make sure zebrad releases lock on cached state directory
    std::thread::sleep(Duration::from_secs(3));

    let zebrad_state_path = test_type
        .zebrad_state_path(test_name)
        .expect("already checked that there is a cached state path");

    let Height(finalized_tip_height) =
        load_tip_height_from_state_directory(network, zebrad_state_path.as_ref()).await?;

    tracing::info!(
        ?finalized_tip_height,
        "finalized tip height from state directory"
    );

    let raw_future_blocks = raw_blocks
        .into_iter()
        .filter_map(|(height, raw_block)| height.gt(&finalized_tip_height).then_some(raw_block))
        .collect();

    Ok(raw_future_blocks)
}
