//! Synchronization Tests: Verifies Zebra's sync behavior with various networks and states.

use zebra_chain::{
    block,
    parameters::Network::{self, Mainnet},
};

use zebra_test::prelude::*;

use crate::common::sync::{
    sync_until, MempoolBehavior, STOP_AT_HEIGHT_REGEX, STOP_ON_LOAD_TIMEOUT,
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
