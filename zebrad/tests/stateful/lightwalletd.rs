use color_eyre::eyre::Result;

use zebra_chain::parameters::Network;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::prelude::*;

use crate::common::{
    launch::spawn_zebrad_for_rpc,
    test_type::TestType::{self, *},
};

use crate::common::lightwalletd::lwd_integration_test;

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state directory path is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
fn lwd_sync_update() -> Result<()> {
    lwd_integration_test(UpdateCachedState)
}

/// Make sure `lightwalletd` can fully sync from genesis using Zebra.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
fn lwd_sync_full() -> Result<()> {
    lwd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    })
}

/// Make sure `lightwalletd` can sync from Zebra, in all available modes.
///
/// Runs the tests in this order:
/// - launch lightwalletd with empty states,
/// - if a cached Zebra state directory path is configured:
///   - run a full sync
/// - if a cached Zebra state directory path is configured:
///   - run a quick update sync,
///   - run a send transaction gRPC test,
///   - run read-only gRPC tests.
///
/// The lightwalletd full, update, and gRPC tests only run with `--features=lightwalletd-grpc-tests`.
///
/// These tests don't work on Windows, so they are always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(not(target_os = "windows"))]
async fn lightwalletd_test_suite() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })?;

    // Only runs when a cached Zebra state directory path is configured with an environment variable.
    lwd_integration_test(UpdateZebraCachedStateNoRpc)?;

    // These tests need the compile-time gRPC feature
    #[cfg(feature = "lightwalletd-grpc-tests")]
    {
        // Do the quick tests first

        // Only runs when a cached Zebra state is configured
        lwd_integration_test(UpdateCachedState)?;

        // Only runs when a cached Zebra state is configured
        crate::common::lightwalletd::wallet_grpc_test::run().await?;

        // Then do the slow tests

        // Only runs when a cached Zebra state is configured.
        // When manually running the test suite, allow cached state in the full sync test.
        lwd_integration_test(FullSyncFromGenesis {
            allow_lightwalletd_cached_state: true,
        })?;

        // Only runs when a cached Zebra state is configured
        crate::common::lightwalletd::send_transaction_test::run().await?;
    }

    Ok(())
}

#[tokio::test]
#[ignore]
async fn lwd_rpc_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateCachedState;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, "lwd_rpc_test", test_type, false)?
    {
        tracing::info!("running fully synced zebrad RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("lightwalletd test must have RPC port");

    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    let client = RpcRequestClient::new(zebra_rpc_address);

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let res = client
        .text_from_call("getblock", r#"["1180900", 0]"#.to_string())
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    let expected_bytes = zebra_test::vectors::MAINNET_BLOCKS
        .get(&1_180_900)
        .expect("test block must exist");
    let expected_hex = hex::encode(expected_bytes);
    assert!(
        res.contains(&expected_hex),
        "response did not contain the desired block: {res}"
    );

    Ok(())
}

/// Test sending transactions using a lightwalletd instance connected to a zebrad instance.
///
/// See [`crate::common::lightwalletd::send_transaction_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(not(target_os = "windows"))]
async fn lwd_rpc_send_tx() -> Result<()> {
    crate::common::lightwalletd::send_transaction_test::run().await
}

/// Test all the rpc methods a wallet connected to lightwalletd can call.
///
/// See [`crate::common::lightwalletd::wallet_grpc_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(not(target_os = "windows"))]
async fn lwd_grpc_wallet() -> Result<()> {
    crate::common::lightwalletd::wallet_grpc_test::run().await
}
