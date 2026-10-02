use color_eyre::eyre::Result;

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};

use crate::common::sync::{
    sync_until_with_peers, GenesisPeer, MempoolBehavior, STOP_AT_HEIGHT_REGEX,
    STOP_ON_LOAD_TIMEOUT, TINY_CHECKPOINT_TEST_HEIGHT, TINY_CHECKPOINT_TIMEOUT,
};

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
///
/// Downloads the genesis block from a local peer, so the test doesn't depend on live peers.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    let _init_guard = zebra_test::init();

    let genesis_peer = GenesisPeer::spawn(&Mainnet)?;

    sync_until_with_peers(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
        Some(genesis_peer.initial_peers()),
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint on testnet.
///
/// The first checkpoint contains a single genesis block.
///
/// Downloads the genesis block from a local peer, so the test doesn't depend on live peers.
// TODO: disabled because testnet is not currently reliable
// #[test]
#[allow(dead_code)]
fn sync_one_checkpoint_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let genesis_peer = GenesisPeer::spawn(&network)?;

    sync_until_with_peers(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &network,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
        Some(genesis_peer.initial_peers()),
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint, restart, and stop on load.
///
/// Downloads the genesis block from a local peer, so the test doesn't depend on live peers.
#[test]
fn restart_stop_at_height() -> Result<()> {
    let _init_guard = zebra_test::init();

    restart_stop_at_height_for_network(Network::Mainnet, TINY_CHECKPOINT_TEST_HEIGHT)?;
    // TODO: disabled because testnet is not currently reliable
    // restart_stop_at_height_for_network(Network::Testnet, TINY_CHECKPOINT_TEST_HEIGHT)?;

    Ok(())
}

fn restart_stop_at_height_for_network(network: Network, height: block::Height) -> Result<()> {
    let genesis_peer = GenesisPeer::spawn(&network)?;

    let reuse_tempdir = sync_until_with_peers(
        height,
        &network,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
        Some(genesis_peer.initial_peers()),
    )?;
    // if stopping corrupts the rocksdb database, zebrad might hang or crash here
    // if stopping does not write the rocksdb database to disk, Zebra will
    // sync, rather than stopping immediately at the configured height
    sync_until_with_peers(
        height,
        &network,
        "state is already at the configured height",
        STOP_ON_LOAD_TIMEOUT,
        reuse_tempdir,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        false,
        Some(genesis_peer.initial_peers()),
    )?;

    Ok(())
}
