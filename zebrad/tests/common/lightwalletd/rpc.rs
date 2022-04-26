//! Lightwalletd gRPC interface and utility functions.

use std::{env, net::SocketAddr};

use tempfile::TempDir;

use zebra_test::{args, net::random_known_port, prelude::*};

use crate::{
    common::{
        config::testdir,
        lightwalletd::{self, LightWalletdTestDirExt, LIGHTWALLETD_TEST_TIMEOUT},
    },
    LIGHTWALLETD_FAILURE_MESSAGES, LIGHTWALLETD_IGNORE_MESSAGES, PROCESS_FAILURE_MESSAGES,
};

tonic::include_proto!("cash.z.wallet.sdk.rpc");

/// Optional environment variable with the cached state for lightwalletd.
///
/// Can be used to speed up the [`sending_transactions_using_lightwalletd`] test, by allowing the
/// test to reuse the cached lightwalletd synchronization data.
const LIGHTWALLETD_DATA_DIR_VAR: &str = "LIGHTWALLETD_DATA_DIR";

/// Type alias for the RPC client to communicate with a lightwalletd instance.
pub type LightwalletdRpcClient =
    lightwalletd::rpc::compact_tx_streamer_client::CompactTxStreamerClient<
        tonic::transport::Channel,
    >;

/// Start a lightwalletd instance with its RPC server functionality enabled.
///
/// Returns the lightwalletd instance and the port number that it is listening for RPC connections.
pub fn spawn_lightwalletd_with_rpc_server(
    zebrad_rpc_address: SocketAddr,
) -> Result<(TestChild<TempDir>, u16)> {
    let lightwalletd_dir = testdir()?.with_lightwalletd_config(zebrad_rpc_address)?;

    let lightwalletd_rpc_port = random_known_port();
    let lightwalletd_rpc_address = format!("127.0.0.1:{lightwalletd_rpc_port}");

    let mut arguments = args!["--grpc-bind-addr": lightwalletd_rpc_address];

    if let Ok(data_dir) = env::var(LIGHTWALLETD_DATA_DIR_VAR) {
        arguments.set_parameter("--data-dir", data_dir);
    }

    let mut lightwalletd = lightwalletd_dir
        .spawn_lightwalletd_child(arguments)?
        .with_timeout(LIGHTWALLETD_TEST_TIMEOUT)
        .with_failure_regex_iter(
            // TODO: replace with a function that returns the full list and correct return type
            LIGHTWALLETD_FAILURE_MESSAGES
                .iter()
                .chain(PROCESS_FAILURE_MESSAGES)
                .cloned(),
            // TODO: some exceptions do not apply to the cached state tests (#3511)
            LIGHTWALLETD_IGNORE_MESSAGES.iter().cloned(),
        );

    lightwalletd.expect_stdout_line_matches("Starting gRPC server")?;
    lightwalletd.expect_stdout_line_matches("Waiting for block")?;

    Ok((lightwalletd, lightwalletd_rpc_port))
}

/// Connect to a lightwalletd RPC instance.
pub async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}
