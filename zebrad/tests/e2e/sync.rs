use color_eyre::eyre::Result;

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};
use zebra_test::prelude::*;

use crate::common::sync::{
    create_cached_database_height, sync_until, MempoolBehavior, LARGE_CHECKPOINT_TEST_HEIGHT,
    LARGE_CHECKPOINT_TIMEOUT, MEDIUM_CHECKPOINT_TEST_HEIGHT, STOP_AT_HEIGHT_REGEX,
    STOP_ON_LOAD_TIMEOUT, SYNC_FINISHED_REGEX, TINY_CHECKPOINT_TEST_HEIGHT,
};

/// Test if `zebrad` can sync some larger checkpoints on mainnet.
///
/// This test depends on real mainnet peers and can take 58 seconds to 23 minutes
/// depending on peer availability. It has a dedicated E2E profile and is excluded
/// from default PR CI.
#[test]
#[ignore]
fn sync_large_checkpoints_empty() -> Result<()> {
    let reuse_tempdir = sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )?;
    // if this sync fails, see the failure notes in `integration::sync::restart_stop_at_height`
    sync_until(
        (LARGE_CHECKPOINT_TEST_HEIGHT - 1).unwrap(),
        &Mainnet,
        "previous state height is greater than the stop height",
        STOP_ON_LOAD_TIMEOUT,
        reuse_tempdir,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        false,
    )?;

    Ok(())
}

// TODO: Replace these public-network checkpoint tests with regtest variants (#9941).
// The removed testnet variants were unreliable because testnet peers and DNS seeders
// were unstable (#1222, #1791).

/// Test if `zebrad` can run side by side with the mempool while syncing checkpoints.
#[test]
#[ignore]
fn sync_large_checkpoints_mempool_mainnet() -> Result<()> {
    sync_until(
        MEDIUM_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ForceActivationAt(TINY_CHECKPOINT_TEST_HEIGHT),
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
#[allow(clippy::print_stderr)]
#[tracing::instrument]
fn full_sync_test(network: Network, _timeout_argument_name: &str) -> Result<()> {
    // # TODO
    //
    // Replace hard-coded values in create_cached_database_height with:
    // - the timeout in the environmental variable
    // - the path from the resolved config (state.cache_dir)
    create_cached_database_height(
        &network,
        // Just keep going until we reach the chain tip
        block::Height::MAX,
        // Use the checkpoints to sync quickly, then do full validation until the chain tip
        true,
        // Finish when we reach the chain tip
        SYNC_FINISHED_REGEX,
    )
}

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
