//! Lightwalletd gRPC interface and utility functions.

use std::{env, net::SocketAddr, path::PathBuf};

use tempfile::TempDir;

use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{
    config::testdir, launch::ZebradTestDirExt, lightwalletd::LightWalletdTestDirExt,
    sync::LIGHTWALLETD_SYNC_FINISHED_REGEX,
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
#[tracing::instrument]
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
#[tracing::instrument]
pub async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}

/// Wait for lightwalletd to sync to Zebra's tip.
///
/// "Adding block" and "Waiting for block" logs stop when `lightwalletd` reaches the tip.
/// But if the logs just stop, we can't tell the difference between a hang and fully synced.
/// So we assume `lightwalletd` will sync and log large groups of blocks,
/// and check for logs with heights near the mainnet tip height.
#[tracing::instrument]
pub fn wait_for_zebrad_and_lightwalletd_tip<
    P: ZebradTestDirExt + std::fmt::Debug + std::marker::Send + 'static,
>(
    mut lightwalletd: TestChild<TempDir>,
    mut zebrad: TestChild<P>,
    test_type: LightwalletdTestType,
) -> Result<(TestChild<TempDir>, TestChild<P>)> {
    let lightwalletd_thread = std::thread::spawn(move || -> Result<_> {
        tracing::info!(?test_type, "waiting for lightwalletd to sync to the tip");

        lightwalletd
            .expect_stdout_line_matches(crate::common::sync::LIGHTWALLETD_SYNC_FINISHED_REGEX)?;

        Ok(lightwalletd)
    });

    // `lightwalletd` syncs can take a long time,
    // so we need to check that `zebrad` has synced to the tip in parallel.
    let lightwalletd_thread_and_zebrad = std::thread::spawn(move || -> Result<_> {
        tracing::info!(?test_type, "waiting for zebrad to sync to the tip");

        while !lightwalletd_thread.is_finished() {
            zebrad.expect_stdout_line_matches(crate::common::sync::SYNC_FINISHED_REGEX)?;
        }

        Ok((lightwalletd_thread, zebrad))
    });

    // Retrieve the child process handles from the threads
    let (lightwalletd_thread, mut zebrad) = lightwalletd_thread_and_zebrad
        .join()
        .unwrap_or_else(|panic_object| std::panic::resume_unwind(panic_object))?;

    let lightwalletd = lightwalletd_thread
        .join()
        .unwrap_or_else(|panic_object| std::panic::resume_unwind(panic_object))?;

    // If we are in sync mempool should activate
    zebrad.expect_stdout_line_matches("activating mempool")?;

    Ok((lightwalletd, zebrad))
}
