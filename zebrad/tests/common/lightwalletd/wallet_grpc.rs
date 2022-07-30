//! Lightwalletd gRPC interface and utility functions.

use std::{env, net::SocketAddr, path::PathBuf};

use tempfile::TempDir;

use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{
    config::testdir, lightwalletd::LightWalletdTestDirExt, sync::LIGHTWALLETD_SYNC_FINISHED_REGEX,
};

use super::LightwalletdTestType;

tonic::include_proto!("cash.z.wallet.sdk.rpc");

/// Type alias for the RPC client to communicate with a lightwalletd instance.
pub type LightwalletdRpcClient =
    compact_tx_streamer_client::CompactTxStreamerClient<tonic::transport::Channel>;

/// Start a lightwalletd instance connected to `zebrad_rpc_address`,
/// using the `lightwalletd_state_path`, with its gRPC server functionality enabled.
///
/// Expects cached state based on the `test_type`.
/// Waits for `lightwalletd` to sync to near the tip, if `wait_for_sync` is true.
///
/// Returns the lightwalletd instance and the port number that it is listening for RPC connections.
pub fn spawn_lightwalletd_with_rpc_server(
    zebrad_rpc_address: SocketAddr,
    lightwalletd_state_path: Option<PathBuf>,
    test_type: LightwalletdTestType,
    wait_for_sync: bool,
) -> Result<(TestChild<TempDir>, u16)> {
    let lightwalletd_dir = testdir()?.with_lightwalletd_config(zebrad_rpc_address)?;

    let lightwalletd_rpc_port = random_known_port();
    let lightwalletd_rpc_address = format!("127.0.0.1:{lightwalletd_rpc_port}");

    let arguments = args!["--grpc-bind-addr": lightwalletd_rpc_address];

    let (lightwalletd_failure_messages, lightwalletd_ignore_messages) =
        test_type.lightwalletd_failure_messages();

    let mut lightwalletd = lightwalletd_dir
        .spawn_lightwalletd_child(lightwalletd_state_path, arguments)?
        .with_timeout(test_type.lightwalletd_timeout())
        .with_failure_regex_iter(lightwalletd_failure_messages, lightwalletd_ignore_messages);

    lightwalletd.expect_stdout_line_matches("Starting gRPC server")?;

    if wait_for_sync {
        lightwalletd.expect_stdout_line_matches(LIGHTWALLETD_SYNC_FINISHED_REGEX)?;
    }

    Ok((lightwalletd, lightwalletd_rpc_port))
}

/// Connect to a lightwalletd RPC instance.
pub async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}
