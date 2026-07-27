use std::{
    path::Path,
    time::{Duration, Instant},
};

use color_eyre::eyre::Result;

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{persistent_test_config, testdir},
    launch::ZebradTestDirExt,
    sync::{
        sync_until, MempoolBehavior, STOP_AT_HEIGHT_REGEX, STOP_ON_LOAD_TIMEOUT,
        TINY_CHECKPOINT_TEST_HEIGHT, TINY_CHECKPOINT_TIMEOUT,
    },
};

/// The maximum time to wait for the persistent state and network caches to be created.
///
/// The peer cache is only written once the node has cacheable peers, and the peer cache updater
/// retries every 20 seconds until its first write, so this needs to cover several attempts on a
/// slow network.
const PERSISTENT_CACHE_CREATION_TIMEOUT: Duration = Duration::from_secs(90);

/// Check that the block state and peer list caches are written to disk.
#[test]
fn persistent_mode() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Wait for both caches to be created and populated, polling instead of sleeping for a
    // fixed time: the peer cache write depends on finding live peers, which can take several
    // attempts on a slow network (#11072).
    let state_dir = testdir.path().join("state");
    let network_dir = testdir.path().join("network");
    let deadline = Instant::now() + PERSISTENT_CACHE_CREATION_TIMEOUT;
    while Instant::now() < deadline
        && !(dir_is_populated(&state_dir) && dir_is_populated(&network_dir))
    {
        std::thread::sleep(Duration::from_secs(1));
    }

    // Kill the node and make sure it was killed
    child.kill(false)?;
    let output = child.wait_with_output()?;
    output.assert_was_killed()?;

    assert_with_context!(
        dir_is_populated(&state_dir),
        &output,
        "state directory missing or empty despite persistent state config"
    );
    assert_with_context!(
        dir_is_populated(&network_dir),
        &output,
        "network directory missing or empty despite persistent network config"
    );

    Ok(())
}

/// Returns `true` if `dir` exists and contains at least one entry.
fn dir_is_populated(dir: &Path) -> bool {
    matches!(
        dir.read_dir().map(|mut entries| entries.next().is_some()),
        Ok(true)
    )
}

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
