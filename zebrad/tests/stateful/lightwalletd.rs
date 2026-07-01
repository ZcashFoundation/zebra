use color_eyre::eyre::Result;

use crate::common::test_type::TestType::*;

use crate::common::lightwalletd::lwd_integration_test;

/// Make sure `lightwalletd` works with Zebra, when both their states are empty.
///
/// This test only runs when the `TEST_LIGHTWALLETD` env var is set.
#[test]
#[ignore]
fn lwd_integration() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })
}

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state directory path is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
#[test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lwd_sync_update() -> Result<()> {
    lwd_integration_test(UpdateCachedState)
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
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
async fn lightwalletd_test_suite() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })?;

    // Only runs when a cached Zebra state directory path is configured with an environment variable.
    lwd_integration_test(UpdateZebraCachedStateNoRpc)?;

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

    Ok(())
}

/// Test sending transactions using a lightwalletd instance connected to a zebrad instance.
///
/// See [`crate::common::lightwalletd::send_transaction_test`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
async fn lwd_rpc_send_tx() -> Result<()> {
    crate::common::lightwalletd::send_transaction_test::run().await
}

/// Test all the rpc methods a wallet connected to lightwalletd can call.
///
/// See [`crate::common::lightwalletd::wallet_grpc_test`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
async fn lwd_grpc_wallet() -> Result<()> {
    crate::common::lightwalletd::wallet_grpc_test::run().await
}
