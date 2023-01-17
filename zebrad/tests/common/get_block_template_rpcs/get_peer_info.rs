//! Tests that `getpeerinfo` RPC method responds with info about at least 1 peer.

use color_eyre::eyre::{Context, Result};

use zebra_chain::parameters::Network;
use zebra_test::args;

use crate::common::{
    config::{random_known_rpc_port_config, testdir},
    launch::ZebradTestDirExt,
    rpc_client::RPCRequestClient,
};

pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    tracing::info!(?network, "running getpeerinfo test using zebrad",);

    let mut config = random_known_rpc_port_config(false)?;
    let rpc_address = config.rpc.listen_addr.unwrap();

    let mut child = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    // Wait until port is open.
    child.expect_stdout_line_matches(&format!("Opened RPC endpoint at {rpc_address}"))?;

    tracing::info!(?rpc_address, "zebrad opened its RPC port",);

    // call `getpeerinfo` RPC method
    let res = RPCRequestClient::new(rpc_address)
        .call("getpeerinfo", "[]".to_string())
        .await?;

    // Test rpc endpoint response
    assert!(res.status().is_success());

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Assert that PeerInfo addresses successfully parse into [`SocketAddr`]
    // for peer_info in peer_info_result {
    //     assert!(
    //         peer_info.addr.parse::<SocketAddr>().is_ok(),
    //         "peer info addr should be a valid SocketAddr",
    //     );
    // }

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
