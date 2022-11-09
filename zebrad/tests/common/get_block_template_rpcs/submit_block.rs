//! Test submitblock RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will get the first few blocks in the non-finalized state
//! (past the MAX_BLOCK_REORG_HEIGHT) via getblock rpc calls, get the finalized tip height
//! of the updated cached state, restart zebra without peers, and submit blocks above the
//! finalized tip height.

use color_eyre::eyre::{Context, Result};

use reqwest::Client;
use zebra_chain::parameters::Network;

use crate::common::{
    cached_state::get_raw_future_blocks,
    launch::{can_spawn_zebrad_for_rpc, spawn_zebrad_for_rpc},
    test_type::TestType,
};

#[allow(clippy::print_stderr)]
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = TestType::UpdateZebraCachedState;
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
