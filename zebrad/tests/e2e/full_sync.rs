//! Full chain sync tests: verifies mainnet/testnet synchronization and optional checkpoint generation.

use std::env;

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};

use zebra_test::prelude::*;

use crate::common::sync::{create_cached_database_height, SYNC_FINISHED_REGEX};

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_mainnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "SYNC_FULL_MAINNET_TIMEOUT_MINUTES")
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_testnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(
        Network::new_default_testnet(),
        "SYNC_FULL_TESTNET_TIMEOUT_MINUTES",
    )
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
#[allow(clippy::print_stderr)]
#[tracing::instrument]
fn full_sync_test(network: Network, timeout_argument_name: &str) -> Result<()> {
    let timeout_argument: Option<u64> = env::var(timeout_argument_name)
        .ok()
        .and_then(|timeout_string| timeout_string.parse().ok());

    // # TODO
    //
    // Replace hard-coded values in create_cached_database_height with:
    // - the timeout in the environmental variable
    // - the path from the resolved config (state.cache_dir)
    if let Some(_timeout_minutes) = timeout_argument {
        create_cached_database_height(
            &network,
            // Just keep going until we reach the chain tip
            block::Height::MAX,
            // Use the checkpoints to sync quickly, then do full validation until the chain tip
            true,
            // Finish when we reach the chain tip
            SYNC_FINISHED_REGEX,
        )
    } else {
        tracing::warn!(
            "Skipped full sync test for {network}, \
            set the {timeout_argument_name:?} environmental variable to run the test",
        );

        Ok(())
    }
}

/// Test `zebra-checkpoints` on mainnet.
///
/// If you want to run this test individually, see the module documentation.
/// See [`common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "zebra-checkpoints")]
async fn generate_checkpoints_mainnet() -> Result<()> {
    crate::common::checkpoints::run(Mainnet).await
}

/// Test `zebra-checkpoints` on testnet.
/// This test might fail if testnet is unstable.
///
/// If you want to run this test individually, see the module documentation.
/// See [`common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "zebra-checkpoints")]
async fn generate_checkpoints_testnet() -> Result<()> {
    crate::common::checkpoints::run(Network::new_default_testnet()).await
}
