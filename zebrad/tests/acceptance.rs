//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! ### Note on port conflict
//!
//! If the test child has a cache or port conflict with another test, or a
//! running zebrad or zcashd, then it will panic. But the acceptance tests
//! expect it to run until it is killed.
//!
//! If these conflicts cause test failures:
//!   - run the tests in an isolated environment,
//!   - run zebrad on a custom cache path and port,
//!   - run zcashd on a custom port.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]
#![allow(dead_code)]
#![allow(clippy::try_err)]

use color_eyre::eyre::Result;
use eyre::WrapErr;
use tempdir::TempDir;

use std::{collections::HashSet, convert::TryInto, env, path::Path, path::PathBuf, time::Duration};

use zebra_chain::{
    block::Height,
    parameters::{
        Network::{self, *},
        NetworkUpgrade,
    },
};
use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::LOCK_FILE_ERROR;
use zebra_test::{
    command::{ContextFrom, TestDirExt},
    prelude::*,
};
use zebrad::config::ZebradConfig;

/// The amount of time we wait after launching `zebrad`.
///
/// Previously, this value was 3 seconds, which caused rare
/// metrics or tracing test failures in Windows CI.
const LAUNCH_DELAY: Duration = Duration::from_secs(10);

fn default_test_config() -> Result<ZebradConfig> {
    let auto_port_ipv4_local = zebra_network::Config {
        listen_addr: "127.0.0.1:0".parse()?,
        ..zebra_network::Config::default()
    };
    let local_ephemeral = ZebradConfig {
        state: zebra_state::Config::ephemeral(),
        network: auto_port_ipv4_local,
        ..ZebradConfig::default()
    };
    Ok(local_ephemeral)
}

fn persistent_test_config() -> Result<ZebradConfig> {
    let mut config = default_test_config()?;
    config.state.ephemeral = false;
    Ok(config)
}

fn testdir() -> Result<TempDir> {
    TempDir::new("zebrad_tests").map_err(Into::into)
}

/// Extension trait for methods on `tempdir::TempDir` for using it as a test
/// directory for `zebrad`.
trait ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `zebrad` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    ///
    /// If there is a config in the test directory, pass it to `zebrad`.
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn with_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Create a config file with the exact contents of `config`, and use it for
    /// all subsequently spawned processes. Returns an error if the config
    /// already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    fn with_exact_config(self, config: &ZebradConfig) -> Result<Self>;

    /// Overwrite any existing config file, and use the newly written config for
    /// all subsequently spawned processes.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn replace_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// `cache_dir` config update helper.
    ///
    /// If needed:
    ///   - set the cache_dir in the config.
    fn cache_config_update_helper(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Config writing helper.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state,
    ///
    /// Then write out the config.
    fn write_config_helper(self, config: &ZebradConfig) -> Result<Self>;
}

impl<T> ZebradTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let path = self.as_ref();
        let default_config_path = path.join("zebrad.toml");

        if default_config_path.exists() {
            let mut extra_args: Vec<_> = vec![
                "-c",
                default_config_path
                    .as_path()
                    .to_str()
                    .expect("Path is valid Unicode"),
            ];
            extra_args.extend_from_slice(args);
            self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad"), &extra_args)
        } else {
            self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad"), args)
        }
    }

    fn with_config(self, config: &mut ZebradConfig) -> Result<Self> {
        self.cache_config_update_helper(config)?
            .write_config_helper(config)
    }

    fn with_exact_config(self, config: &ZebradConfig) -> Result<Self> {
        self.write_config_helper(config)
    }

    fn replace_config(self, config: &mut ZebradConfig) -> Result<Self> {
        use std::fs;
        use std::io::ErrorKind;

        // Remove any existing config before writing a new one
        let dir = self.as_ref();
        let config_file = dir.join("zebrad.toml");
        match fs::remove_file(config_file) {
            Ok(()) => {}
            // If the config file doesn't exist, that's ok
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => Err(e)?,
        }

        self.cache_config_update_helper(config)?
            .write_config_helper(config)
    }

    fn cache_config_update_helper(self, config: &mut ZebradConfig) -> Result<Self> {
        if !config.state.ephemeral {
            let dir = self.as_ref();
            let cache_dir = dir.join("state");
            config.state.cache_dir = cache_dir;
        }

        Ok(self)
    }

    fn write_config_helper(self, config: &ZebradConfig) -> Result<Self> {
        use std::fs;
        use std::io::Write;

        let dir = self.as_ref();

        if !config.state.ephemeral {
            let cache_dir = dir.join("state");
            fs::create_dir_all(&cache_dir)?;
        } else {
            fs::create_dir_all(&dir)?;
        }

        let config_file = dir.join("zebrad.toml");
        fs::File::create(config_file)?.write_all(toml::to_string(&config)?.as_bytes())?;

        Ok(self)
    }
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let child = testdir()?
        .with_config(&mut default_test_config()?)?
        .spawn_child(&["generate"])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

/// Panics if `$pred` is false, with an error report containing:
///   * context from `$source`, and
///   * an optional wrapper error, using `$fmt_arg`+ as a format string and
///     arguments.
macro_rules! assert_with_context {
    ($pred:expr, $source:expr) => {
        if !$pred {
            use color_eyre::Section as _;
            use color_eyre::SectionExt as _;
            use zebra_test::command::ContextFrom as _;
            let report = color_eyre::eyre::eyre!("failed assertion")
                .section(stringify!($pred).header("Predicate:"))
                .context_from($source);

            panic!("Error: {:?}", report);
        }
    };
    ($pred:expr, $source:expr, $($fmt_arg:tt)+) => {
        if !$pred {
            use color_eyre::Section as _;
            use color_eyre::SectionExt as _;
            use zebra_test::command::ContextFrom as _;
            let report = color_eyre::eyre::eyre!("failed assertion")
                .section(stringify!($pred).header("Predicate:"))
                .context_from($source)
                .wrap_err(format!($($fmt_arg)+));

            panic!("Error: {:?}", report);
        }
    };
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(&["generate", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(&["generate", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let child = testdir.spawn_child(&["generate", "-o"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    // Valid
    let child =
        testdir.spawn_child(&["generate", "-o", generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        testdir.path().exists(),
        &output,
        "test temp directory not found"
    );
    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(&["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line haves the version
    output.stdout_contains(r"zebrad [0-9].[0-9].[0-9]")?;

    // Make sure we are in help by looking usage string
    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // The subcommand "argument" wasn't recognized.
    let child = testdir.spawn_child(&["help", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let child = testdir.spawn_child(&["help", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config()?)?;

    let mut child = testdir.spawn_child(&["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad$")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(&["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(&["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn persistent_mode() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config()?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(&["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "state directory empty despite persistent state config"
    );

    Ok(())
}

/// The cache_dir config used in the ephemeral mode tests
#[derive(Debug, PartialEq, Eq)]
enum EphemeralConfig {
    /// the cache_dir config is left at its default value
    Default,
    /// the cache_dir config is set to a path in the tempdir
    MisconfiguredCacheDir,
}

/// The check performed by the ephemeral mode tests
#[derive(Debug, PartialEq, Eq)]
enum EphemeralCheck {
    /// an existing directory is not deleted
    ExistingDirectory,
    /// a missing directory is not created
    MissingDirectory,
}

#[test]
fn ephemeral_existing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::ExistingDirectory)
}

#[test]
fn ephemeral_missing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::MissingDirectory)
}

#[test]
fn misconfigured_ephemeral_existing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::ExistingDirectory,
    )
}

#[test]
fn misconfigured_ephemeral_missing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::MissingDirectory,
    )
}

fn ephemeral(cache_dir_config: EphemeralConfig, cache_dir_check: EphemeralCheck) -> Result<()> {
    use std::fs;
    use std::io::ErrorKind;

    zebra_test::init();

    let mut config = default_test_config()?;
    let run_dir = TempDir::new("zebrad_tests")?;

    let ignored_cache_dir = run_dir.path().join("state");
    if cache_dir_config == EphemeralConfig::MisconfiguredCacheDir {
        // Write a configuration that sets both the cache_dir and ephemeral options
        config.state.cache_dir = ignored_cache_dir.clone();
    }
    if cache_dir_check == EphemeralCheck::ExistingDirectory {
        // We set the cache_dir config to a newly created empty temp directory,
        // then make sure that it is empty after the test
        fs::create_dir(&ignored_cache_dir)?;
    }

    let mut child = run_dir
        .path()
        .with_config(&mut config)?
        .spawn_child(&["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let expected_run_dir_file_names = match cache_dir_check {
        // we created the state directory, so it should still exist
        EphemeralCheck::ExistingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect("ignored_cache_dir should still exist")
                    .count()
                    == 0,
                &output,
                "ignored_cache_dir not empty for ephemeral {:?} {:?}: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );

            ["state", "zebrad.toml"].iter()
        }

        // we didn't create the state directory, so it should not exist
        EphemeralCheck::MissingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect_err("ignored_cache_dir should not exist")
                    .kind()
                    == ErrorKind::NotFound,
                &output,
                "unexpected creation of ignored_cache_dir for ephemeral {:?} {:?}: the cache dir exists and contains these files: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );

            ["zebrad.toml"].iter()
        }
    };

    let expected_run_dir_file_names = expected_run_dir_file_names.map(Into::into).collect();
    let run_dir_file_names = run_dir
        .path()
        .read_dir()
        .expect("run_dir should still exist")
        .map(|dir_entry| dir_entry.expect("run_dir is readable").file_name())
        // ignore directory list order, because it can vary based on the OS and filesystem
        .collect::<HashSet<_>>();

    assert_with_context!(
        run_dir_file_names == expected_run_dir_file_names,
        &output,
        "run_dir not empty for ephemeral {:?} {:?}: expected {:?}, actual: {:?}",
        cache_dir_config,
        cache_dir_check,
        expected_run_dir_file_names,
        run_dir_file_names
    );

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(&[])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(&["version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_matches(r"^zebrad [0-9].[0-9].[0-9]-[A-Za-z]*.[0-9]\n$")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(&["version", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(&["version", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn valid_generated_config_test() -> Result<()> {
    // Unlike the other tests, these tests can not be run in parallel, because
    // they use the generated config. So parallel execution can cause port and
    // cache conflicts.
    valid_generated_config("start", r"Starting zebrad$")?;

    Ok(())
}

fn valid_generated_config(command: &str, expected_output: &str) -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    // Generate configuration in temp dir path
    let child =
        testdir.spawn_child(&["generate", "-o", generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    // Run command using temp dir and kill it after a few seconds
    let mut child = testdir.spawn_child(&[command])?;
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(expected_output)?;

    // [Note on port conflict](#Note on port conflict)
    output.assert_was_killed().wrap_err("Possible port or cache conflict. Are there other acceptance test, zebrad, or zcashd processes running?")?;

    assert_with_context!(
        testdir.path().exists(),
        &output,
        "test temp directory not found"
    );
    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    Ok(())
}

const LARGE_CHECKPOINT_TEST_HEIGHT: Height =
    Height((zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP * 2) as u32);

const STOP_AT_HEIGHT_REGEX: &str = "stopping at configured height";

const STOP_ON_LOAD_TIMEOUT: Duration = Duration::from_secs(5);
// usually it's much shorter than this
const SMALL_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(30);
const LARGE_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(180);

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_until(
        Height(0),
        Mainnet,
        STOP_AT_HEIGHT_REGEX,
        SMALL_CHECKPOINT_TIMEOUT,
        None,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint on testnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_until(
        Height(0),
        Testnet,
        STOP_AT_HEIGHT_REGEX,
        SMALL_CHECKPOINT_TIMEOUT,
        None,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint, restart, and stop on load.
#[test]
fn restart_stop_at_height() -> Result<()> {
    let reuse_tempdir = sync_until(
        Height(0),
        Mainnet,
        STOP_AT_HEIGHT_REGEX,
        SMALL_CHECKPOINT_TIMEOUT,
        None,
    )?;
    // if stopping corrupts the rocksdb database, zebrad might hang here
    // if stopping does not sync the rocksdb database, the logs will contain OnCommit
    sync_until(
        Height(0),
        Mainnet,
        "state is already at the configured height",
        STOP_ON_LOAD_TIMEOUT,
        Some(reuse_tempdir),
    )?;

    Ok(())
}

/// Test if `zebrad` can sync some larger checkpoints on mainnet.
///
/// This test might fail or timeout on slow or unreliable networks,
/// so we don't run it by default. It also takes a lot longer than
/// our 10 second target time for default tests.
#[test]
#[ignore]
fn sync_large_checkpoints_mainnet() -> Result<()> {
    let reuse_tempdir = sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
    )?;
    // if this sync fails, see the failure notes in `restart_stop_at_height`
    sync_until(
        (LARGE_CHECKPOINT_TEST_HEIGHT - 1).unwrap(),
        Mainnet,
        "previous state height is greater than the stop height",
        STOP_ON_LOAD_TIMEOUT,
        Some(reuse_tempdir),
    )?;

    Ok(())
}

// Todo: We had a `sync_large_checkpoints_testnet` here but it was removed because
// the testnet is unreliable(#1222). Enable after we have more testnet instances(#1791).

/// Sync `network` until `zebrad` reaches `height`, and ensure that
/// the output contains `stop_regex`. If `reuse_tempdir` is supplied,
/// use it as the test's temporary directory.
///
/// If `stop_regex` is encountered before the process exits, kills the
/// process, and mark the test as successful, even if `height` has not
/// been reached.
///
/// On success, returns the associated `TempDir`. Returns an error if
/// the child exits or `timeout` elapses before `regex` is found.
///
/// If your test environment does not have network access, skip
/// this test by setting the `ZEBRA_SKIP_NETWORK_TESTS` env var.
fn sync_until(
    height: Height,
    network: Network,
    stop_regex: &str,
    timeout: Duration,
    reuse_tempdir: Option<TempDir>,
) -> Result<TempDir> {
    zebra_test::init();

    if env::var_os("ZEBRA_SKIP_NETWORK_TESTS").is_some() {
        // This message is captured by the test runner, use
        // `cargo test -- --nocapture` to see it.
        eprintln!("Skipping network test because '$ZEBRA_SKIP_NETWORK_TESTS' is set.");
        return testdir();
    }

    // Use a persistent state, so we can handle large syncs
    let mut config = persistent_test_config()?;
    // TODO: add convenience methods?
    config.network.network = network;
    config.state.debug_stop_at_height = Some(height.0);

    let tempdir = if let Some(reuse_tempdir) = reuse_tempdir {
        reuse_tempdir.replace_config(&mut config)?
    } else {
        testdir()?.with_config(&mut config)?
    };

    let mut child = tempdir.spawn_child(&["start"])?.with_timeout(timeout);

    let network = format!("network: {},", network);
    child.expect_stdout(&network)?;
    child.expect_stdout(stop_regex)?;
    child.kill()?;

    Ok(child.dir)
}

fn cached_sapling_test_config() -> Result<ZebradConfig> {
    let mut config = persistent_test_config()?;
    config.consensus.checkpoint_sync = true;
    config.state.cache_dir = "/zebrad-cache".into();
    Ok(config)
}

fn create_cached_database_height(network: Network, height: Height) -> Result<()> {
    println!("Creating cached database");
    // 8 hours
    let timeout = Duration::from_secs(60 * 60 * 8);

    // Use a persistent state, so we can handle large syncs
    let mut config = cached_sapling_test_config()?;
    // TODO: add convenience methods?
    config.network.network = network;
    config.state.debug_stop_at_height = Some(height.0);

    let dir = PathBuf::from("/zebrad-cache");
    let mut child = dir
        .with_exact_config(&config)?
        .spawn_child(&["start"])?
        .with_timeout(timeout)
        .bypass_test_capture(true);

    let network = format!("network: {},", network);
    child.expect_stdout(&network)?;
    child.expect_stdout(STOP_AT_HEIGHT_REGEX)?;
    child.kill()?;

    Ok(())
}

fn create_cached_database(network: Network) -> Result<()> {
    let height = NetworkUpgrade::Sapling.activation_height(network).unwrap();
    create_cached_database_height(network, height)
}

fn sync_past_sapling(network: Network) -> Result<()> {
    let height = NetworkUpgrade::Sapling.activation_height(network).unwrap() + 1200;
    create_cached_database_height(network, height.unwrap())
}

// These tests are ignored because they're too long running to run during our
// traditional CI, and they depend on persistent state that cannot be made
// available in github actions or google cloud build. Instead we run these tests
// directly in a vm we spin up on google compute engine, where we can mount
// drives populated by the first two tests, snapshot those drives, and then use
// those to more quickly run the second two tests.

// Sync up to the sapling activation height on mainnet and stop.
#[cfg_attr(feature = "test_sync_to_sapling_mainnet", test)]
fn sync_to_sapling_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    create_cached_database(network).unwrap();
}

// Sync to the sapling activation height testnet and stop.
#[cfg_attr(feature = "test_sync_to_sapling_testnet", test)]
fn sync_to_sapling_testnet() {
    zebra_test::init();
    let network = Testnet;
    create_cached_database(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the last checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near Sapling
/// activation on mainnet. If the state has already synced past Sapling
/// activation by 1200 blocks, it will fail.
#[cfg_attr(feature = "test_sync_past_sapling_mainnet", test)]
fn sync_past_sapling_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    sync_past_sapling(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the last checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near Sapling
/// activation on testnet. If the state has already synced past Sapling
/// activation by 1200 blocks, it will fail.
#[cfg_attr(feature = "test_sync_past_sapling_testnet", test)]
fn sync_past_sapling_testnet() {
    zebra_test::init();
    let network = Testnet;
    sync_past_sapling(network).unwrap();
}

/// Returns a random port number from the ephemeral port range.
///
/// Does not check if the port is already in use. It's impossible to do this
/// check in a reliable, cross-platform way.
///
/// ## Usage
///
/// If you want a once-off random unallocated port, use
/// `random_unallocated_port`. Don't use this function if you don't need
/// to - it has a small risk of port conflcits.
///
/// Use this function when you need to use the same random port multiple
/// times. For example: setting up both ends of a connection, or re-using
/// the same port multiple times.
fn random_known_port() -> u16 {
    // Use the intersection of the IANA/Windows/macOS ephemeral port range,
    // and the Linux ephemeral port range:
    //   - https://en.wikipedia.org/wiki/Ephemeral_port#Range
    // excluding ports less than 53500, to avoid:
    //   - typical Hyper-V reservations up to 52000:
    //      - https://github.com/googlevr/gvr-unity-sdk/issues/1002
    //      - https://github.com/docker/for-win/issues/3171
    //   - the MOM-Clear port 51515
    //      - https://docs.microsoft.com/en-us/troubleshoot/windows-server/networking/service-overview-and-network-port-requirements
    //   - the LDAP Kerberos byte-swapped reservation 53249
    //      - https://docs.microsoft.com/en-us/troubleshoot/windows-server/identity/ldap-kerberos-server-reset-tcp-sessions
    //   - macOS and Windows sequential ephemeral port allocations,
    //     starting from 49152:
    //      - https://dataplane.org/ephemeralports.html
    rand::thread_rng().gen_range(53500, 60999)
}

/// Returns the "magic" port number that tells the operating system to
/// choose a random unallocated port.
///
/// The OS chooses a different port each time it opens a connection or
/// listener with this magic port number.
///
/// ## Usage
///
/// See the usage note for `random_known_port`.
#[allow(dead_code)]
fn random_unallocated_port() -> u16 {
    0
}

#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use hyper::Client;

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{}", port);
    let url = format!("http://{}", endpoint);

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config()?;
    config.metrics.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = TempDir::new("zebrad_tests")?.with_config(&mut config)?;
    let child = dir.spawn_child(&["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client = Client::new();

    // Test metrics endpoint
    let res = client.get(url.try_into().expect("url is valid")).await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await;
    let (body, mut child) = child.kill_on_error(body)?;
    assert!(
        std::str::from_utf8(&body)
            .expect("metrics response is valid UTF-8")
            .contains("metrics snapshot"),
        "metrics exporter returns data in the expected format"
    );

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure metrics was started
    output.stdout_contains(format!(r"Opened metrics endpoint at {}", endpoint).as_str())?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

#[tokio::test]
async fn tracing_endpoint() -> Result<()> {
    use hyper::{Body, Client, Request};

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{}", port);
    let url_default = format!("http://{}", endpoint);
    let url_filter = format!("{}/filter", url_default);

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config()?;
    config.tracing.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = TempDir::new("zebrad_tests")?.with_config(&mut config)?;
    let child = dir.spawn_child(&["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client = Client::new();

    // Test tracing endpoint
    let res = client
        .get(url_default.try_into().expect("url_default is valid"))
        .await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await;
    let (body, child) = child.kill_on_error(body)?;
    assert!(std::str::from_utf8(&body).unwrap().contains(
        "This HTTP endpoint allows dynamic control of the filter applied to\ntracing events."
    ));

    // Set a filter and make sure it was changed
    let request = Request::post(url_filter.clone())
        .body(Body::from("zebrad=debug"))
        .unwrap();
    let post = client.request(request).await;
    let (_post, child) = child.kill_on_error(post)?;

    let tracing_res = client
        .get(url_filter.try_into().expect("url_filter is valid"))
        .await;
    let (tracing_res, child) = child.kill_on_error(tracing_res)?;
    assert!(tracing_res.status().is_success());
    let tracing_body = hyper::body::to_bytes(tracing_res).await;
    let (tracing_body, mut child) = child.kill_on_error(tracing_body)?;
    assert!(std::str::from_utf8(&tracing_body)
        .unwrap()
        .contains("zebrad=debug"));

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_contains(format!(r"Opened tracing endpoint at {}", endpoint).as_str())?;
    // Todo: Match some trace level messages from output

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test will start 2 zebrad nodes one after the other using the same Zcash listener.
/// It is expected that the first node spawned will get exclusive use of the port.
/// The second node will panic with the Zcash listener conflict hint added in #1535.
#[test]
fn zebra_zcash_listener_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created network listen_addr
    let mut config = default_test_config()?;
    config.network.listen_addr = listen_addr.parse().unwrap();
    let dir1 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;
    let regex1 = format!(r"Opened Zcash protocol endpoint at {}", listen_addr);

    // From another folder create a configuration with the same listener.
    // `network.listen_addr` will be the same in the 2 nodes.
    // (But since the config is ephemeral, they will have different state paths.)
    let dir2 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same metrics listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash metrics
/// conflict hint added in #1535.
#[test]
fn zebra_metrics_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created metrics endpoint_addr
    let mut config = default_test_config()?;
    config.metrics.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;
    let regex1 = format!(r"Opened metrics endpoint at {}", listen_addr);

    // From another folder create a configuration with the same endpoint.
    // `metrics.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same tracing listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash tracing
/// conflict hint added in #1535.
#[test]
fn zebra_tracing_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created tracing endpoint_addr
    let mut config = default_test_config()?;
    config.tracing.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;
    let regex1 = format!(r"Opened tracing endpoint at {}", listen_addr);

    // From another folder create a configuration with the same endpoint.
    // `tracing.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = TempDir::new("zebrad_tests")?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same state directory, but different Zcash
/// listener ports. The first node should get exclusive access to the database.
/// The second node will panic with the Zcash state conflict hint added in #1535.
#[test]
fn zebra_state_conflict() -> Result<()> {
    zebra_test::init();

    // A persistent config has a fixed temp state directory, but asks the OS to
    // automatically choose an unused port
    let mut config = persistent_test_config()?;
    let dir_conflict = TempDir::new("zebrad_tests")?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let regex = if cfg!(unix) {
        let mut dir_conflict_full = PathBuf::new();
        dir_conflict_full.push(dir_conflict.path());
        dir_conflict_full.push("state");
        dir_conflict_full.push("state");
        dir_conflict_full.push(format!(
            "v{}",
            zebra_state::constants::DATABASE_FORMAT_VERSION
        ));
        dir_conflict_full.push(config.network.network.to_string().to_lowercase());
        format!(
            "Opened Zebra state cache at {}",
            dir_conflict_full.display()
        )
    } else {
        String::from("Opened Zebra state cache at ")
    };

    check_config_conflict(
        dir_conflict.path(),
        regex.as_str(),
        dir_conflict.path(),
        LOCK_FILE_ERROR.as_str(),
    )?;

    Ok(())
}

/// Launch a node in `first_dir`, wait a few seconds, then launch a node in
/// `second_dir`. Check that the first node's stdout contains
/// `first_stdout_regex`, and the second node's stderr contains
/// `second_stderr_regex`.
fn check_config_conflict<T, U>(
    first_dir: T,
    first_stdout_regex: &str,
    second_dir: U,
    second_stderr_regex: &str,
) -> Result<()>
where
    T: ZebradTestDirExt,
    U: ZebradTestDirExt,
{
    // Start the first node
    let node1 = first_dir.spawn_child(&["start"])?;

    // Wait a bit to spawn the second node, we want the first fully started.
    std::thread::sleep(LAUNCH_DELAY);

    // Spawn the second node
    let node2 = second_dir.spawn_child(&["start"]);
    let (node2, mut node1) = node1.kill_on_error(node2)?;

    // Wait a few seconds and kill first node.
    // Second node is terminated by panic, no need to kill.
    std::thread::sleep(LAUNCH_DELAY);
    let node1_kill_res = node1.kill();
    let (_, node2) = node2.kill_on_error(node1_kill_res)?;

    // In node1 we want to check for the success regex
    // If there are any errors, we also want to print the node2 output.
    let output1 = node1.wait_with_output();
    // This mut is only used on some platforms, due to #1781.
    #[allow(unused_mut)]
    let (output1, mut node2) = node2.kill_on_error(output1)?;

    // node2 should have panicked due to a conflict. Kill it here anyway, so it
    // doesn't outlive the test on error.
    //
    // This code doesn't work on Windows or macOS. It's cleanup code that only
    // runs when node2 doesn't panic as expected. So it's ok to skip it.
    // See #1781.
    #[cfg(target_os = "linux")]
    if node2.is_running() {
        use color_eyre::eyre::eyre;
        return node2
            .kill_on_error::<(), _>(Err(eyre!(
                "conflicted node2 was still running, but the test expected a panic"
            )))
            .context_from(&output1)
            .map(|_| ());
    }
    // Now we're sure both nodes are dead, and we have both their outputs
    let output2 = node2.wait_with_output().context_from(&output1)?;

    // Look for the success regex
    output1
        .stdout_contains(first_stdout_regex)
        .context_from(&output2)?;
    use color_eyre::Help;
    output1
        .assert_was_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output2)?;

    // In the second node we look for the conflict regex
    output2
        .stderr_contains(second_stderr_regex)
        .context_from(&output1)?;
    output2
        .assert_was_not_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output1)?;

    Ok(())
}
