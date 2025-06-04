//! Tests that `getpeerinfo` RPC method responds with info about at least 1 peer.

use color_eyre::eyre::{eyre, Context, Result};

use zebra_chain::parameters::Network;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::client::types::PeerInfo;

use crate::common::{
    launch::{can_spawn_zebrad_for_test_type, spawn_zebrad_for_rpc},
    test_type::TestType,
};

pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_type = TestType::LaunchWithEmptyState {
        launches_lightwalletd: false,
    };
    let test_name = "get_peer_info_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_test_type(test_name, test_type, true) {
        return Ok(());
    }

    tracing::info!(?network, "running getpeerinfo test using zebrad",);

    let (mut zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, true)?
            .expect("Already checked zebra state path with can_spawn_zebrad_for_test_type");

    let rpc_address = zebra_rpc_address.expect("getpeerinfo test must have RPC port");

    // Wait until port is open.
    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {rpc_address}"))?;

    tracing::info!(?rpc_address, "zebrad opened its RPC port",);

    // call `getpeerinfo` RPC method
    let peer_info_result: Vec<PeerInfo> = RpcRequestClient::new(rpc_address)
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    assert!(
        !peer_info_result.is_empty(),
        "getpeerinfo should return info for at least 1 peer"
    );

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
