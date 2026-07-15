//! Tests that `getpeerinfo` RPC method responds with info about at least 1 peer.

use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};

use zebra_chain::parameters::Network;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::client::PeerInfo;

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

    let peer_info = tokio::time::timeout(Duration::from_secs(2 * 60), async {
        loop {
            let peer_info: Vec<PeerInfo> = RpcRequestClient::new(rpc_address)
                .json_result_from_call("getpeerinfo", "[]")
                .await
                .map_err(|err| eyre!(err))?;

            if !peer_info.is_empty() {
                return Ok::<Vec<PeerInfo>, color_eyre::eyre::Report>(peer_info);
            }

            tracing::info!("waiting for getpeerinfo to return at least 1 peer");

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for getpeerinfo to return at least 1 peer")??;

    tracing::info!(?peer_info, "getpeerinfo returned peer info");

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}
