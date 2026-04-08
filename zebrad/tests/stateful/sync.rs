use color_eyre::eyre::Result;

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};
use zebra_test::prelude::*;

use crate::common::{
    sync::{create_cached_database_height, STOP_AT_HEIGHT_REGEX, SYNC_FINISHED_REGEX},
    test_type::TestType::*,
};

#[tracing::instrument]
fn create_cached_database(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height();
    let checkpoint_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit checkpoint-verified request");

    create_cached_database_height(
        &network,
        height,
        // Use checkpoints to increase sync performance while caching the database
        true,
        // Check that we're still using checkpoints when we finish the cached sync
        &checkpoint_stop_regex,
    )
}

#[tracing::instrument]
fn sync_past_mandatory_checkpoint(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height() + (32_257 + 1200);
    let full_validation_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit contextually-verified request");

    create_cached_database_height(
        &network,
        height.unwrap(),
        // Test full validation by turning checkpoints off
        false,
        // Check that we're doing full validation when we finish the cached sync
        &full_validation_stop_regex,
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

/// Sync up to the mandatory checkpoint height on mainnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_mainnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Mainnet)
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_testnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Network::new_default_testnet())
}

/// Helper function for sync to checkpoint tests
fn sync_to_mandatory_checkpoint_for_network(network: Network) -> Result<()> {
    let _init_guard = zebra_test::init();
    create_cached_database(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_mainnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    sync_past_mandatory_checkpoint(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on testnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    sync_past_mandatory_checkpoint(network)
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

/// Make sure `zebrad` can sync from peers, but don't actually launch `lightwalletd`.
///
/// This test only runs when a persistent cached state directory path is configured
/// (for example, by setting `ZEBRA_STATE__CACHE_DIR`).
///
/// This test might work on Windows.
#[test]
#[ignore]
fn sync_update_mainnet() -> Result<()> {
    crate::common::lightwalletd::lwd_integration_test(UpdateZebraCachedStateNoRpc)
}
