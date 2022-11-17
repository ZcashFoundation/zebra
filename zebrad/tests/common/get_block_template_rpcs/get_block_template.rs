//! Test getblocktemplate RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will call getblocktemplate.

use color_eyre::eyre::{eyre, Context, Result};

use zebra_chain::parameters::Network;

use crate::common::{
    launch::{can_spawn_zebrad_for_rpc, spawn_zebrad_for_rpc},
    rpc_client::RPCRequestClient,
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    test_type::TestType,
};

pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let test_name = "get_block_template_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_rpc(test_name, test_type) {
        return Ok(());
    }

    tracing::info!(
        ?network,
        ?test_type,
        "running getblocktemplate test using zebrad",
    );

    let should_sync = true;
    let (zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
            .ok_or_else(|| eyre!("getblocktemplate test requires a cached state"))?;

    let rpc_address = zebra_rpc_address.expect("test type must have RPC port");

    let mut zebrad = check_sync_logs_until(
        zebrad,
        network,
        SYNC_FINISHED_REGEX,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
    )?;

    let rpc_client = RPCRequestClient::new(rpc_address);

    let getblocktemplate_response = &rpc_client
        .call("getblocktemplate", "[]".to_string())
        .await?;

    assert!(getblocktemplate_response.status().is_success());

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
