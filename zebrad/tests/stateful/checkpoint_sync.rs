//! Checkpoint sync tests

use zebra_chain::parameters::Network::{self, *};

use zebra_test::prelude::*;

use crate::common::sync::{create_cached_database_height, STOP_AT_HEIGHT_REGEX};

// These tests are ignored because they're too long running to run during our
// traditional CI, and they depend on persistent state that cannot be made
// available in github actions or google cloud build. Instead we run these tests
// directly in a vm we spin up on google compute engine, where we can mount
// drives populated by the sync_to_mandatory_checkpoint tests, snapshot those drives,
// and then use them to more quickly run the sync_past_mandatory_checkpoint tests.

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
    use std::env;

    // Skip unless explicitly enabled
    if env::var("TEST_SYNC_TO_CHECKPOINT").is_err() {
        return Ok(());
    }

    let _init_guard = zebra_test::init();
    create_cached_database(network)
}

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

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_mainnet() -> Result<()> {
    // Skip unless explicitly enabled
    if std::env::var("TEST_SYNC_PAST_CHECKPOINT").is_err() {
        tracing::warn!(
            "Skipped sync_past_mandatory_checkpoint_mainnet, set the TEST_SYNC_PAST_CHECKPOINT environmental variable to run the test"
        );
        return Ok(());
    }
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
    // Skip unless explicitly enabled
    if std::env::var("TEST_SYNC_PAST_CHECKPOINT").is_err() {
        tracing::warn!(
            "Skipped sync_past_mandatory_checkpoint_testnet, set the TEST_SYNC_PAST_CHECKPOINT environmental variable to run the test"
        );
        return Ok(());
    }
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    sync_past_mandatory_checkpoint(network)
}

/// Helper function for sync past checkpoint tests
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
