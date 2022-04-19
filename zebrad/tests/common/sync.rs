//! `zebrad` sync-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{path::PathBuf, time::Duration};

use color_eyre::eyre::Result;
use tempfile::TempDir;

use zebra_chain::{block::Height, parameters::Network};
use zebrad::{components::sync, config::ZebradConfig};

use zebra_test::{args, prelude::*};

use super::{
    config::{persistent_test_config, testdir},
    launch::ZebradTestDirExt,
};

pub const TINY_CHECKPOINT_TEST_HEIGHT: Height = Height(0);
pub const MEDIUM_CHECKPOINT_TEST_HEIGHT: Height =
    Height(zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP as u32);
pub const LARGE_CHECKPOINT_TEST_HEIGHT: Height =
    Height((zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP * 2) as u32);

pub const STOP_AT_HEIGHT_REGEX: &str = "stopping at configured height";

/// The text that should be logged when the initial sync finishes at the estimated chain tip.
///
/// This message is only logged if:
/// - we have reached the estimated chain tip,
/// - we have synced all known checkpoints,
/// - the syncer has stopped downloading lots of blocks, and
/// - we are regularly downloading some blocks via the syncer or block gossip.
pub const SYNC_FINISHED_REGEX: &str = "finished initial sync to chain tip, using gossiped blocks";

/// The maximum amount of time Zebra should take to reload after shutting down.
///
/// This should only take a second, but sometimes CI VMs or RocksDB can be slow.
pub const STOP_ON_LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The maximum amount of time Zebra should take to sync a few hundred blocks.
///
/// Usually the small checkpoint is much shorter than this.
pub const TINY_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(120);

/// The maximum amount of time Zebra should take to sync a thousand blocks.
pub const LARGE_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(180);

/// The test sync height where we switch to using the default lookahead limit.
///
/// Most tests only download a few blocks. So tests default to the minimum lookahead limit,
/// to avoid downloading extra blocks, and slowing down the test.
///
/// But if we're going to be downloading lots of blocks, we use the default lookahead limit,
/// so that the sync is faster. This can increase the RAM needed for tests.
pub const MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD: Height =
    Height(3 * sync::DEFAULT_LOOKAHEAD_LIMIT as u32);

/// What the expected behavior of the mempool is for a test that uses [`sync_until`].
pub enum MempoolBehavior {
    /// The mempool should be forced to activate at a certain height, for debug purposes.
    ///
    /// [`sync_until`] will kill `zebrad` after it logs mempool activation,
    /// then the `stop_regex`.
    ForceActivationAt(Height),

    /// The mempool should be automatically activated.
    ///
    /// [`sync_until`] will kill `zebrad` after it logs mempool activation,
    /// then the `stop_regex`.
    ShouldAutomaticallyActivate,

    /// The mempool should not become active during the test.
    ///
    /// # Correctness
    ///
    /// Unlike the other mempool behaviours, `zebrad` must stop after logging the stop regex,
    /// without being killed by [`sync_until`] test harness.
    ///
    /// Since it needs to collect all the output,
    /// the test harness can't kill `zebrad` after it logs the `stop_regex`.
    ShouldNotActivate,
}

impl MempoolBehavior {
    /// Return the height value that the mempool should be enabled at, if available.
    pub fn enable_at_height(&self) -> Option<u32> {
        match self {
            MempoolBehavior::ForceActivationAt(height) => Some(height.0),
            MempoolBehavior::ShouldAutomaticallyActivate | MempoolBehavior::ShouldNotActivate => {
                None
            }
        }
    }

    /// Returns `true` if the mempool should activate,
    /// either by forced or automatic activation.
    pub fn require_activation(&self) -> bool {
        self.require_forced_activation() || self.require_automatic_activation()
    }

    /// Returns `true` if the mempool should be forcefully activated at a specified height.
    pub fn require_forced_activation(&self) -> bool {
        matches!(self, MempoolBehavior::ForceActivationAt(_))
    }

    /// Returns `true` if the mempool should automatically activate.
    pub fn require_automatic_activation(&self) -> bool {
        matches!(self, MempoolBehavior::ShouldAutomaticallyActivate)
    }

    /// Returns `true` if the mempool should not activate.
    #[allow(dead_code)]
    pub fn require_no_activation(&self) -> bool {
        matches!(self, MempoolBehavior::ShouldNotActivate)
    }
}

/// Sync on `network` until `zebrad` reaches `height`, or until it logs `stop_regex`.
///
/// If `stop_regex` is encountered before the process exits, kills the
/// process, and mark the test as successful, even if `height` has not
/// been reached. To disable the height limit, and just stop at `stop_regex`,
/// use `Height::MAX` for the `height`.
///
/// # Test Settings
///
/// If `reuse_tempdir` is supplied, use it as the test's temporary directory.
///
/// If `height` is higher than the mandatory checkpoint,
/// configures `zebrad` to preload the Zcash parameters.
/// If it is lower, skips the parameter preload.
///
/// Configures `zebrad` to debug-enable the mempool based on `mempool_behavior`,
/// then check the logs for the expected `mempool_behavior`.
///
/// If `checkpoint_sync` is true, configures `zebrad` to use as many checkpoints as possible.
/// If it is false, only use the mandatory checkpoints.
///
/// If `check_legacy_chain` is true, make sure the logs contain the legacy chain check.
///
/// If your test environment does not have network access, skip
/// this test by setting the `ZEBRA_SKIP_NETWORK_TESTS` env var.
///
/// # Test Status
///
/// On success, returns the associated `TempDir`. Returns an error if
/// the child exits or `timeout` elapses before `stop_regex` is found.
#[allow(clippy::too_many_arguments)]
pub fn sync_until(
    height: Height,
    network: Network,
    stop_regex: &str,
    timeout: Duration,
    // Test Settings
    // TODO: turn these into an argument struct
    reuse_tempdir: impl Into<Option<TempDir>>,
    mempool_behavior: MempoolBehavior,
    checkpoint_sync: bool,
    check_legacy_chain: bool,
) -> Result<TempDir> {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return testdir();
    }

    let reuse_tempdir = reuse_tempdir.into();

    // Use a persistent state, so we can handle large syncs
    let mut config = persistent_test_config()?;
    config.network.network = network;
    config.state.debug_stop_at_height = Some(height.0);
    config.mempool.debug_enable_at_height = mempool_behavior.enable_at_height();
    config.consensus.checkpoint_sync = checkpoint_sync;

    // Download the parameters at launch, if we're going to need them later.
    if height > network.mandatory_checkpoint_height() {
        config.consensus.debug_skip_parameter_preload = false;
    }

    // Use the default lookahead limit if we're syncing lots of blocks.
    // (Most tests use a smaller limit to minimise redundant block downloads.)
    if height > MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD {
        config.sync.lookahead_limit = sync::DEFAULT_LOOKAHEAD_LIMIT;
    }

    let tempdir = if let Some(reuse_tempdir) = reuse_tempdir {
        reuse_tempdir.replace_config(&mut config)?
    } else {
        testdir()?.with_config(&mut config)?
    };

    let mut child = tempdir.spawn_child(args!["start"])?.with_timeout(timeout);

    let network = format!("network: {},", network);

    if mempool_behavior.require_activation() {
        // require that the mempool activated,
        // checking logs as they arrive

        child.expect_stdout_line_matches(&network)?;

        if check_legacy_chain {
            child.expect_stdout_line_matches("starting legacy chain check")?;
            child.expect_stdout_line_matches("no legacy chain found")?;
        }

        // before the stop regex, expect mempool activation
        if mempool_behavior.require_forced_activation() {
            child.expect_stdout_line_matches("enabling mempool for debugging")?;
        }
        child.expect_stdout_line_matches("activating mempool")?;

        // then wait for the stop log, which must happen after the mempool becomes active
        child.expect_stdout_line_matches(stop_regex)?;

        // make sure the child process is dead
        // if it has already exited, ignore that error
        let _ = child.kill();

        Ok(child.dir.take().expect("dir was not already taken"))
    } else {
        // Require that the mempool didn't activate,
        // checking the entire `zebrad` output after it exits.
        //
        // # Correctness
        //
        // Unlike the other mempool behaviours, `zebrad` must stop after logging the stop regex,
        // without being killed by [`sync_until`] test harness.
        //
        // Since it needs to collect all the output,
        // the test harness can't kill `zebrad` after it logs the `stop_regex`.
        assert!(
            height.0 < 2_000_000,
            "zebrad must exit by itself, so we can collect all the output",
        );
        let output = child.wait_with_output()?;

        output.stdout_line_contains(&network)?;

        if check_legacy_chain {
            output.stdout_line_contains("starting legacy chain check")?;
            output.stdout_line_contains("no legacy chain found")?;
        }

        // check it did not activate or use the mempool
        assert!(output.stdout_line_contains("activating mempool").is_err());
        assert!(output
            .stdout_line_contains("sending mempool transaction broadcast")
            .is_err());

        // check it logged the stop regex before exiting
        output.stdout_line_matches(stop_regex)?;

        // check exited by itself, successfully
        output.assert_was_not_killed()?;
        let output = output.assert_success()?;

        Ok(output.dir.expect("wait_with_output sets dir"))
    }
}

/// Returns a test config for caching Zebra's state up to the mandatory checkpoint.
pub fn cached_mandatory_checkpoint_test_config() -> Result<ZebradConfig> {
    let mut config = persistent_test_config()?;
    config.state.cache_dir = "/zebrad-cache".into();

    // To get to the mandatory checkpoint, we need to sync lots of blocks.
    // (Most tests use a smaller limit to minimise redundant block downloads.)
    //
    // If we're syncing past the checkpoint with cached state, we don't need the extra lookahead.
    // But the extra downloaded blocks shouldn't slow down the test that much,
    // and testing with the defaults gives us better test coverage.
    config.sync.lookahead_limit = sync::DEFAULT_LOOKAHEAD_LIMIT;

    Ok(config)
}

/// Create or update a cached state for `network`, stopping at `height`.
///
/// # Test Settings
///
/// If `debug_skip_parameter_preload` is true, configures `zebrad` to preload the Zcash parameters.
/// If it is false, skips the parameter preload.
///
/// If `checkpoint_sync` is true, configures `zebrad` to use as many checkpoints as possible.
/// If it is false, only use the mandatory checkpoints.
///
/// If `check_legacy_chain` is true, make sure the logs contain the legacy chain check.
///
/// The test passes when `zebrad` logs the `stop_regex`.
/// Typically this is `STOP_AT_HEIGHT_REGEX`,
/// with an extra check for checkpoint or full validation.
///
/// This test ignores the `ZEBRA_SKIP_NETWORK_TESTS` env var.
///
/// # Test Status
///
/// Returns an error if the child exits or the fixed timeout elapses
/// before `STOP_AT_HEIGHT_REGEX` is found.
#[allow(clippy::print_stderr)]
pub fn create_cached_database_height(
    network: Network,
    height: Height,
    debug_skip_parameter_preload: bool,
    checkpoint_sync: bool,
    stop_regex: &str,
) -> Result<()> {
    eprintln!("creating cached database");

    // 16 hours
    let timeout = Duration::from_secs(60 * 60 * 16);

    // Use a persistent state, so we can handle large syncs
    let mut config = cached_mandatory_checkpoint_test_config()?;
    // TODO: add convenience methods?
    config.network.network = network;
    config.state.debug_stop_at_height = Some(height.0);
    config.consensus.debug_skip_parameter_preload = debug_skip_parameter_preload;
    config.consensus.checkpoint_sync = checkpoint_sync;

    let dir = PathBuf::from("/zebrad-cache");
    let mut child = dir
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .with_timeout(timeout)
        .bypass_test_capture(true);

    let network = format!("network: {},", network);
    child.expect_stdout_line_matches(&network)?;

    child.expect_stdout_line_matches("starting legacy chain check")?;
    child.expect_stdout_line_matches("no legacy chain found")?;

    child.expect_stdout_line_matches(stop_regex)?;

    child.kill()?;

    Ok(())
}
