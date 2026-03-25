//! `zebrad` sync-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{path::PathBuf, time::Duration};

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

/// The text that should be logged when Zebra's initial sync finishes at the estimated chain tip.
///
/// This message is only logged if:
/// - we have reached the estimated chain tip,
/// - we have synced all known checkpoints,
/// - the syncer has stopped downloading lots of blocks, and
/// - we are regularly downloading some blocks via the syncer or block gossip.
///
/// The trailing `\.` is required, so the regex finds the fractional percentage,
/// and the other integers on that line are ignored.
pub const SYNC_FINISHED_REGEX: &str =
    r"finished initial sync to chain tip, using gossiped blocks .*sync_percent.*=.*100\.";

/// The text that should be logged every time Zebra checks the sync progress.
#[cfg(feature = "lightwalletd-grpc-tests")]
pub const SYNC_PROGRESS_REGEX: &str = r"sync_percent";

/// The text that should be logged when Zebra loads its compiled-in checkpoints.
#[cfg(feature = "zebra-checkpoints")]
pub const CHECKPOINT_VERIFIER_REGEX: &str =
    r"initializing block verifier router.*max_checkpoint_height.*=.*Height";

/// The maximum amount of time Zebra should take to reload after shutting down.
///
/// This should only take a second, but sometimes CI VMs or RocksDB can be slow.
pub const STOP_ON_LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The maximum amount of time Zebra should take to sync a few hundred blocks.
///
/// Usually the small checkpoint is much shorter than this.
//
// Tempoaraily increased to 4 minutes to get more diagnostic info in failed tests.
// TODO: reduce to 120 when #6506 is fixed
pub const TINY_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(240);

/// The maximum amount of time Zebra should take to sync a thousand blocks.
//
// Tempoaraily increased to 4 minutes to get more diagnostic info in failed tests.
// TODO: reduce to 180 when #6506 is fixed
pub const LARGE_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(240);

/// The maximum time to wait for Zebrad to synchronize up to the chain tip starting from a
/// partially synchronized state.
///
/// The partially synchronized state is expected to be close to the tip, so this timeout can be
/// lower than what's expected for a full synchronization. However, a value that's too short may
/// cause the test to fail.
pub const FINISH_PARTIAL_SYNC_TIMEOUT: Duration = Duration::from_secs(11 * 60 * 60);

/// The maximum time to wait for Zebrad to synchronize up to the chain tip starting from the
/// genesis block.
pub const FINISH_FULL_SYNC_TIMEOUT: Duration = Duration::from_secs(72 * 60 * 60);

/// The test sync height where we switch to using the default lookahead limit.
///
/// Most tests only download a few blocks. So tests default to the minimum lookahead limit,
/// to avoid downloading extra blocks, and slowing down the test.
///
/// But if we're going to be downloading lots of blocks, we use the default lookahead limit,
/// so that the sync is faster. This can increase the RAM needed for tests.
pub const MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD: Height =
    Height(3 * sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT as u32);

/// What the expected behavior of the mempool is for a test that uses [`sync_until`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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
    #[allow(dead_code)]
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
/// Configures `zebrad` to debug-enable the mempool based on `mempool_behavior`,
/// then check the logs for the expected `mempool_behavior`.
///
/// If `checkpoint_sync` is true, configures `zebrad` to use as many checkpoints as possible.
/// If it is false, only use the mandatory checkpoints.
///
/// If `check_legacy_chain` is true, make sure the logs contain the legacy chain check.
///
/// If your test environment does not have network access, skip
/// this test by setting the `SKIP_NETWORK_TESTS` env var.
///
/// # Test Status
///
/// On success, returns the associated `TempDir`. Returns an error if
/// the child exits or `timeout` elapses before `stop_regex` is found.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(reuse_tempdir))]
pub fn sync_until(
    height: Height,
    network: &Network,
    stop_regex: &str,
    timeout: Duration,
    // Test Settings
    // TODO: turn these into an argument struct
    reuse_tempdir: impl Into<Option<TempDir>>,
    mempool_behavior: MempoolBehavior,
    checkpoint_sync: bool,
    check_legacy_chain: bool,
) -> Result<TempDir> {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return testdir();
    }

    let reuse_tempdir = reuse_tempdir.into();

    // Use a persistent state, so we can handle large syncs
    let mut config = persistent_test_config(network)?;
    config.state.debug_stop_at_height = Some(height.0);
    config.mempool.debug_enable_at_height = mempool_behavior.enable_at_height();
    config.consensus.checkpoint_sync = checkpoint_sync;

    // Use the default lookahead limit if we're syncing lots of blocks.
    // (Most tests use a smaller limit to minimise redundant block downloads.)
    if height > MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD {
        config.sync.checkpoint_verify_concurrency_limit =
            sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;
    }

    let tempdir = if let Some(reuse_tempdir) = reuse_tempdir {
        reuse_tempdir.replace_config(&mut config)?
    } else {
        testdir()?.with_config(&mut config)?
    };

    let child = tempdir.spawn_child(args!["start"])?.with_timeout(timeout);

    let network_log = format!("network: {network},");

    if mempool_behavior.require_activation() {
        // require that the mempool activated,
        // checking logs as they arrive

        let mut child = check_sync_logs_until(
            child,
            network,
            stop_regex,
            mempool_behavior,
            check_legacy_chain,
        )?;

        // make sure the child process is dead
        // if it has already exited, ignore that error
        child.kill(true)?;
        let dir = child.dir.take().expect("dir was not already taken");
        // Wait for zebrad to fully terminate to ensure database lock is released.
        child.wait_with_output()?;
        std::thread::sleep(std::time::Duration::from_secs(3));

        Ok(dir)
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

        output.stdout_line_contains(&network_log)?;

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

/// Check sync logs on `network` until `zebrad` logs `stop_regex`.
///
/// ## Test Settings
///
/// Checks the logs for the expected `mempool_behavior`.
///
/// If `check_legacy_chain` is true, make sure the logs contain the legacy chain check.
///
/// ## Test Status
///
/// Returns the provided `zebrad` [`TestChild`] when `stop_regex` is encountered.
///
/// Returns an error if the child exits or `timeout` elapses before `stop_regex` is found.
#[tracing::instrument(skip(zebrad))]
pub fn check_sync_logs_until(
    mut zebrad: TestChild<TempDir>,
    network: &Network,
    stop_regex: &str,
    // Test Settings
    mempool_behavior: MempoolBehavior,
    check_legacy_chain: bool,
) -> Result<TestChild<TempDir>> {
    zebrad.expect_stdout_line_matches(format!("network: {network},"))?;

    if check_legacy_chain {
        zebrad.expect_stdout_line_matches("starting legacy chain check")?;
        zebrad.expect_stdout_line_matches("no legacy chain found")?;

        zebrad.expect_stdout_line_matches("starting state checkpoint validation")?;
        // TODO: what if the mempool is enabled for debugging before this finishes?
        zebrad.expect_stdout_line_matches("finished state checkpoint validation")?;
    }

    // before the stop regex, expect mempool activation
    if mempool_behavior.require_forced_activation() {
        zebrad.expect_stdout_line_matches("enabling mempool for debugging")?;
    }
    zebrad.expect_stdout_line_matches("activating mempool")?;

    // then wait for the stop log, which must happen after the mempool becomes active
    zebrad.expect_stdout_line_matches(stop_regex)?;

    Ok(zebrad)
}

/// Returns the cache directory for Zebra's state, as configured with [`ZebradConfig::load`] with config-rs.
///
/// Uses the resolved configuration from [`ZebradConfig::load(None)`](ZebradConfig::load), which
/// incorporates defaults, optional TOML, and environment overrides.
fn get_zebra_cached_state_dir() -> PathBuf {
    ZebradConfig::load(None)
        .map(|c| c.state.cache_dir)
        .unwrap_or_else(|_| "/zebrad-cache".into())
}

/// Returns a test config for caching Zebra's state up to the mandatory checkpoint.
pub fn cached_mandatory_checkpoint_test_config(network: &Network) -> Result<ZebradConfig> {
    let mut config = persistent_test_config(network)?;
    config.state.cache_dir = get_zebra_cached_state_dir();

    // To get to the mandatory checkpoint, we need to sync lots of blocks.
    // (Most tests use a smaller limit to minimise redundant block downloads.)
    //
    // If we're syncing past the checkpoint with cached state, we don't need the extra lookahead.
    // But the extra downloaded blocks shouldn't slow down the test that much,
    // and testing with the defaults gives us better test coverage.
    config.sync.checkpoint_verify_concurrency_limit = sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;

    Ok(config)
}

/// Create or update a cached state for `network`, stopping at `height`.
///
/// # Test Settings
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
/// This test ignores the `SKIP_NETWORK_TESTS` env var.
///
/// # Test Status
///
/// Returns an error if the child exits or the fixed timeout elapses
/// before `STOP_AT_HEIGHT_REGEX` is found.
#[allow(clippy::print_stderr)]
#[tracing::instrument]
pub fn create_cached_database_height(
    network: &Network,
    height: Height,
    checkpoint_sync: bool,
    stop_regex: &str,
) -> Result<()> {
    eprintln!("creating cached database");

    // Use a persistent state, so we can handle large syncs
    let mut config = cached_mandatory_checkpoint_test_config(network)?;
    // TODO: add convenience methods?
    config.state.debug_stop_at_height = Some(height.0);
    config.consensus.checkpoint_sync = checkpoint_sync;

    let dir = get_zebra_cached_state_dir();
    let mut child = dir
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .with_timeout(FINISH_FULL_SYNC_TIMEOUT)
        .bypass_test_capture(true);

    let network = format!("network: {network},");
    child.expect_stdout_line_matches(network)?;

    child.expect_stdout_line_matches("starting legacy chain check")?;
    child.expect_stdout_line_matches("no legacy chain found")?;

    child.expect_stdout_line_matches(stop_regex)?;

    // make sure the child process is dead
    // if it has already exited, ignore that error
    child.kill(true)?;

    Ok(())
}
