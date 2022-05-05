//! Lightwalletd gRPC interface and utility functions.

use std::{env, net::SocketAddr};

use tempfile::TempDir;

use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{config::testdir, lightwalletd::LightWalletdTestDirExt};

use super::LightwalletdTestType;

tonic::include_proto!("cash.z.wallet.sdk.rpc");

/// Type alias for the RPC client to communicate with a lightwalletd instance.
pub type LightwalletdRpcClient =
    compact_tx_streamer_client::CompactTxStreamerClient<tonic::transport::Channel>;

/// Start a lightwalletd instance with its RPC server functionality enabled.
///
/// Returns the lightwalletd instance and the port number that it is listening for RPC connections.
pub fn spawn_lightwalletd_with_rpc_server(
    zebrad_rpc_address: SocketAddr,
    wait_for_blocks: bool,
) -> Result<(TestChild<TempDir>, u16)> {
    // We're using cached Zebra state here, so this test type is the most similar
    let test_type = LightwalletdTestType::UpdateCachedState;

    let lightwalletd_dir = testdir()?.with_lightwalletd_config(zebrad_rpc_address)?;

    let lightwalletd_rpc_port = random_known_port();
    let lightwalletd_rpc_address = format!("127.0.0.1:{lightwalletd_rpc_port}");

    let arguments = args!["--grpc-bind-addr": lightwalletd_rpc_address];

    let (lightwalletd_failure_messages, lightwalletd_ignore_messages) =
        test_type.lightwalletd_failure_messages();

    let mut lightwalletd = lightwalletd_dir
        .spawn_lightwalletd_child(test_type.lightwalletd_state_path(), arguments)?
        .with_timeout(test_type.lightwalletd_timeout())
        .with_failure_regex_iter(lightwalletd_failure_messages, lightwalletd_ignore_messages);

    lightwalletd.expect_stdout_line_matches("Starting gRPC server")?;
    if wait_for_blocks {
        lightwalletd.expect_stdout_line_matches("Waiting for block")?;
    }
    Ok((lightwalletd, lightwalletd_rpc_port))
}

/// Connect to a lightwalletd RPC instance.
pub async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}
