//! Synchronization Tests: Verifies Zebra's sync behavior with various networks and states.

use zebra_chain::{
    block,
    parameters::Network::{self, Mainnet},
};

use zebra_test::prelude::*;

use crate::common::sync::{
    sync_until, MempoolBehavior, LARGE_CHECKPOINT_TEST_HEIGHT, LARGE_CHECKPOINT_TIMEOUT,
    MEDIUM_CHECKPOINT_TEST_HEIGHT, STOP_AT_HEIGHT_REGEX, STOP_ON_LOAD_TIMEOUT,
    TINY_CHECKPOINT_TEST_HEIGHT, TINY_CHECKPOINT_TIMEOUT,
};

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint on testnet.
///
/// The first checkpoint contains a single genesis block.
// TODO: disabled because testnet is not currently reliable
// #[test]
#[allow(dead_code)]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &Network::new_default_testnet(),
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint, restart, and stop on load.
#[test]
fn restart_stop_at_height() -> Result<()> {
    let _init_guard = zebra_test::init();

    restart_stop_at_height_for_network(Network::Mainnet, TINY_CHECKPOINT_TEST_HEIGHT)?;
    // TODO: disabled because testnet is not currently reliable
    // restart_stop_at_height_for_network(Network::Testnet, TINY_CHECKPOINT_TEST_HEIGHT)?;

    Ok(())
}

fn restart_stop_at_height_for_network(network: Network, height: block::Height) -> Result<()> {
    let reuse_tempdir = sync_until(
        height,
        &network,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )?;
    // if stopping corrupts the rocksdb database, zebrad might hang or crash here
    // if stopping does not write the rocksdb database to disk, Zebra will
    // sync, rather than stopping immediately at the configured height
    sync_until(
        height,
        &network,
        "state is already at the configured height",
        STOP_ON_LOAD_TIMEOUT,
        reuse_tempdir,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        false,
    )?;

    Ok(())
}

/// Test if `zebrad` can activate the mempool on mainnet.
/// Debug activation happens after committing the genesis block.
#[test]
fn activate_mempool_mainnet() -> Result<()> {
    sync_until(
        block::Height(TINY_CHECKPOINT_TEST_HEIGHT.0 + 1),
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ForceActivationAt(TINY_CHECKPOINT_TEST_HEIGHT),
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync some larger checkpoints on mainnet.
///
/// This test might fail or timeout on slow or unreliable networks,
/// so we don't run it by default. It also takes a lot longer than
/// our 10 second target time for default tests.
#[test]
#[ignore]
fn sync_large_checkpoints_empty() -> Result<()> {
    // Skip unless explicitly enabled
    if std::env::var("TEST_LARGE_CHECKPOINTS").is_err() {
        tracing::warn!(
            "Skipped sync_large_checkpoints_empty, set the TEST_LARGE_CHECKPOINTS environmental variable to run the test"
        );
        return Ok(());
    }

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
    // if this sync fails, see the failure notes in `restart_stop_at_height`
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

// TODO: We had `sync_large_checkpoints_empty` and `sync_large_checkpoints_mempool_testnet`,
// but they were removed because the testnet is unreliable (#1222).
// We should re-add them after we have more testnet instances (#1791).

/// Test if `zebrad` can run side by side with the mempool.
/// This is done by running the mempool and syncing some checkpoints.
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
