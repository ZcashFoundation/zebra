//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! ## Note on port conflict
//!
//! If the test child has a cache or port conflict with another test, or a
//! running zebrad or zcashd, then it will panic. But the acceptance tests
//! expect it to run until it is killed.
//!
//! If these conflicts cause test failures:
//!   - run the tests in an isolated environment,
//!   - run zebrad on a custom cache path and port,
//!   - run zcashd on a custom port.
//!
//! ## Failures due to Configured Network Interfaces or Network Connectivity
//!
//! If your test environment does not have any IPv6 interfaces configured, skip IPv6 tests
//! by setting the `ZEBRA_SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, IPv4 localhost is not on `127.0.0.1`,
//! or you have poor network connectivity,
//! skip all the network tests by setting the `ZEBRA_SKIP_NETWORK_TESTS` environmental variable.

use color_eyre::{
    eyre::{Result, WrapErr},
    Help,
};
use tempfile::TempDir;

use std::{
    collections::HashSet, convert::TryInto, env, net::SocketAddr, path::Path, path::PathBuf,
    time::Duration,
};

use zebra_chain::{
    block::Height,
    parameters::Network::{self, *},
};
use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::LOCK_FILE_ERROR;
use zebra_test::{
    command::{ContextFrom, TestDirExt},
    net::random_known_port,
    prelude::*,
};
use zebrad::{
    components::{mempool, sync},
    config::{SyncSection, TracingSection, ZebradConfig},
};

/// The amount of time we wait after launching `zebrad`.
///
/// Previously, this value was 3 seconds, which caused rare
/// metrics or tracing test failures in Windows CI.
const LAUNCH_DELAY: Duration = Duration::from_secs(10);

/// Returns a config with:
/// - a Zcash listener on an unused port on IPv4 localhost, and
/// - an ephemeral state,
/// - the minimum syncer lookahead limit, and
/// - shorter task intervals, to improve test coverage.
fn default_test_config() -> Result<ZebradConfig> {
    const TEST_DURATION: Duration = Duration::from_secs(30);

    let network = zebra_network::Config {
        // The OS automatically chooses an unused port.
        listen_addr: "127.0.0.1:0".parse()?,
        crawl_new_peer_interval: TEST_DURATION,
        ..zebra_network::Config::default()
    };

    let sync = SyncSection {
        // Avoid downloading unnecessary blocks.
        lookahead_limit: sync::MIN_LOOKAHEAD_LIMIT,
        ..SyncSection::default()
    };

    let mempool = mempool::Config {
        eviction_memory_time: TEST_DURATION,
        ..mempool::Config::default()
    };

    let consensus = zebra_consensus::Config {
        debug_skip_parameter_preload: true,
        ..zebra_consensus::Config::default()
    };

    let force_use_color = !matches!(
        env::var("ZEBRA_FORCE_USE_COLOR"),
        Err(env::VarError::NotPresent)
    );
    let tracing = TracingSection {
        force_use_color,
        ..TracingSection::default()
    };

    let config = ZebradConfig {
        network,
        state: zebra_state::Config::ephemeral(),
        sync,
        mempool,
        consensus,
        tracing,
        ..ZebradConfig::default()
    };

    Ok(config)
}

fn persistent_test_config() -> Result<ZebradConfig> {
    let mut config = default_test_config()?;
    config.state.ephemeral = false;
    Ok(config)
}

fn testdir() -> Result<TempDir> {
    tempfile::Builder::new()
        .prefix("zebrad_tests")
        .tempdir()
        .map_err(Into::into)
}

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `zebrad`.
trait ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    // Zebra methods

    /// Spawn `zebrad` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    ///
    /// If there is a config in the test directory, pass it to `zebrad`.
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned `zebrad` processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn with_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Create a config file with the exact contents of `config`, and use it for
    /// all subsequently spawned `zebrad` processes. Returns an error if the config
    /// already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    fn with_exact_config(self, config: &ZebradConfig) -> Result<Self>;

    /// Overwrite any existing `zebrad` config file, and use the newly written config for
    /// all subsequently spawned processes.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn replace_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// `cache_dir` config update helper for `zebrad`.
    ///
    /// If needed:
    ///   - set the cache_dir in the config.
    fn cache_config_update_helper(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Config writing helper for `zebrad`.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state,
    ///
    /// Then write out the config.
    fn write_config_helper(self, config: &ZebradConfig) -> Result<Self>;

    // lightwalletd methods

    /// Spawn `lightwalletd` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    ///
    /// By default, launch a working test instance with logging, and avoid port conflicts.
    ///
    /// # Panics
    ///
    /// If there is no lightwalletd config in the test directory.
    fn spawn_lightwalletd_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned `lightwalletd` processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config
    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self>;
}

impl<T> ZebradTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let dir = self.as_ref();
        let default_config_path = dir.join("zebrad.toml");

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

    fn spawn_lightwalletd_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let dir = self.as_ref().to_owned();
        let default_config_path = dir.join("lightwalletd-zcash.conf");

        assert!(
            default_config_path.exists(),
            "lightwalletd requires a config"
        );

        // By default, launch a working test instance with logging,
        // and avoid port conflicts.
        let mut extra_args: Vec<_> = vec![
            // the fake zcashd conf we just wrote
            "--zcash-conf-path",
            default_config_path
                .as_path()
                .to_str()
                .expect("Path is valid Unicode"),
            // the lightwalletd cache directory
            //
            // TODO: create a sub-directory for lightwalletd
            "--data-dir",
            dir.to_str().expect("Path is valid Unicode"),
            // log to standard output
            //
            // TODO: if lightwalletd needs to run on Windows,
            //       work out how to log to the terminal on all platforms
            "--log-file",
            "/dev/stdout",
            // let the OS choose a random available wallet client port
            "--grpc-bind-addr",
            "127.0.0.1:0",
            "--http-bind-addr",
            "127.0.0.1:0",
            // don't require a TLS certificate for the HTTP server
            "--no-tls-very-insecure",
        ];
        extra_args.extend_from_slice(args);

        self.spawn_child_with_command("lightwalletd", &extra_args)
    }

    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self> {
        use std::fs;
        use std::io::Write;

        let lightwalletd_config = format!(
            "\
            rpcbind={}\n\
            rpcport={}\n\
            ",
            zebra_rpc_listener.ip(),
            zebra_rpc_listener.port(),
        );

        let dir = self.as_ref();
        fs::create_dir_all(dir)?;

        let config_file = dir.join("lightwalletd-zcash.conf");
        fs::File::create(config_file)?.write_all(lightwalletd_config.as_bytes())?;

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
    output.stdout_line_contains("# Default configuration for zebrad")?;

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

/// Is `s` a valid `zebrad` version string?
///
/// Trims whitespace before parsing the version.
///
/// Returns false if the version is invalid, or if there is anything else on the
/// line that contains the version. In particular, this check will fail if `s`
/// includes any terminal formatting.
fn is_zebrad_version(s: &str) -> bool {
    semver::Version::parse(s.replace("zebrad", "").trim()).is_ok()
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(&["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The first line should have the version
    output.any_output_line(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

    // Make sure we are in help by looking usage string
    output.stdout_line_contains("USAGE:")?;

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

    output.stdout_line_contains("Starting zebrad")?;

    // Make sure the command passed the legacy chain check
    output.stdout_line_contains("starting legacy chain check")?;
    output.stdout_line_contains("no legacy chain found")?;

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
    let run_dir = testdir()?;

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

    output.stdout_line_contains("USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(&["version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The output should only contain the version
    output.output_check(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

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
    valid_generated_config("start", "Starting zebrad")?;

    Ok(())
}

fn valid_generated_config(command: &str, expect_stdout_line_contains: &str) -> Result<()> {
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

    output.stdout_line_contains(expect_stdout_line_contains)?;

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

const TINY_CHECKPOINT_TEST_HEIGHT: Height = Height(0);
const MEDIUM_CHECKPOINT_TEST_HEIGHT: Height =
    Height(zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP as u32);
const LARGE_CHECKPOINT_TEST_HEIGHT: Height =
    Height((zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP * 2) as u32);

const STOP_AT_HEIGHT_REGEX: &str = "stopping at configured height";

/// The text that should be logged when the initial sync finishes at the estimated chain tip.
///
/// This message is only logged if:
/// - we have reached the estimated chain tip,
/// - we have synced all known checkpoints,
/// - the syncer has stopped downloading lots of blocks, and
/// - we are regularly downloading some blocks via the syncer or block gossip.
const SYNC_FINISHED_REGEX: &str = "finished initial sync to chain tip, using gossiped blocks";

/// The maximum amount of time Zebra should take to reload after shutting down.
///
/// This should only take a second, but sometimes CI VMs or RocksDB can be slow.
const STOP_ON_LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The maximum amount of time Zebra should take to sync a few hundred blocks.
///
/// Usually the small checkpoint is much shorter than this.
const TINY_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(120);

/// The maximum amount of time Zebra should take to sync a thousand blocks.
const LARGE_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(180);

/// The test sync height where we switch to using the default lookahead limit.
///
/// Most tests only download a few blocks. So tests default to the minimum lookahead limit,
/// to avoid downloading extra blocks, and slowing down the test.
///
/// But if we're going to be downloading lots of blocks, we use the default lookahead limit,
/// so that the sync is faster. This can increase the RAM needed for tests.
const MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD: Height = Height(3 * sync::DEFAULT_LOOKAHEAD_LIMIT as u32);

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
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
#[test]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        Testnet,
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
    zebra_test::init();

    restart_stop_at_height_for_network(Network::Mainnet, TINY_CHECKPOINT_TEST_HEIGHT)?;
    restart_stop_at_height_for_network(Network::Testnet, TINY_CHECKPOINT_TEST_HEIGHT)?;

    Ok(())
}

fn restart_stop_at_height_for_network(network: Network, height: Height) -> Result<()> {
    let reuse_tempdir = sync_until(
        height,
        network,
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
        network,
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
        Height(TINY_CHECKPOINT_TEST_HEIGHT.0 + 1),
        Mainnet,
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
fn sync_large_checkpoints_mainnet() -> Result<()> {
    let reuse_tempdir = sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
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
        Mainnet,
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

// TODO: We had `sync_large_checkpoints_testnet` and `sync_large_checkpoints_mempool_testnet`,
// but they were removed because the testnet is unreliable (#1222).
// We should re-add them after we have more testnet instances (#1791).

/// Test if `zebrad` can run side by side with the mempool.
/// This is done by running the mempool and syncing some checkpoints.
#[test]
#[ignore]
fn sync_large_checkpoints_mempool_mainnet() -> Result<()> {
    sync_until(
        MEDIUM_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
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

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_mainnet() {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "FULL_SYNC_MAINNET_TIMEOUT_MINUTES").expect("unexpected test failure");
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_testnet() {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Testnet, "FULL_SYNC_TESTNET_TIMEOUT_MINUTES").expect("unexpected test failure");
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
fn full_sync_test(network: Network, timeout_argument_name: &'static str) -> Result<()> {
    let timeout_argument: Option<u64> = env::var(timeout_argument_name)
        .ok()
        .and_then(|timeout_string| timeout_string.parse().ok());

    if let Some(timeout_minutes) = timeout_argument {
        sync_until(
            Height::MAX,
            network,
            SYNC_FINISHED_REGEX,
            Duration::from_secs(60 * timeout_minutes),
            None,
            MempoolBehavior::ShouldAutomaticallyActivate,
            // Use checkpoints to increase full sync performance, and test default Zebra behaviour.
            // (After the changes to the default config in #2368.)
            //
            // TODO: if full validation performance improves, do another test with checkpoint_sync off
            true,
            true,
        )
        .map(|_| ())
    } else {
        tracing::info!(
            ?network,
            "skipped full sync test, \
             set the {:?} environmental variable to run the test",
            timeout_argument_name,
        );

        Ok(())
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
fn sync_until(
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

    let mut child = tempdir.spawn_child(&["start"])?.with_timeout(timeout);

    let network = format!("network: {},", network);
    child.expect_stdout_line_matches(&network)?;

    if check_legacy_chain {
        child.expect_stdout_line_matches("starting legacy chain check")?;
        child.expect_stdout_line_matches("no legacy chain found")?;
    }

    if mempool_behavior.is_forced_activation() {
        child.expect_stdout_line_matches("enabling mempool for debugging")?;
        child.expect_stdout_line_matches("activating mempool")?;

        // make sure zebra is running with the mempool
        child.expect_stdout_line_matches("verified checkpoint range")?;
    }

    child.expect_stdout_line_matches(stop_regex)?;

    // make sure mempool behaves as expected when we don't explicitly enable it
    if !mempool_behavior.is_forced_activation() {
        // if there is no matching line, the `expect_stdout_line_matches` error kills the `zebrad` child.
        // the error is delayed until the test timeout, or until the child reaches the stop height and exits.
        let mempool_is_activated = child
            .expect_stdout_line_matches("activating mempool")
            .is_ok();

        let mempool_check = match mempool_behavior {
            MempoolBehavior::ShouldAutomaticallyActivate if !mempool_is_activated => {
                Some("mempool did not activate as expected")
            }
            MempoolBehavior::ShouldNotActivate if mempool_is_activated => Some(
                "unexpected mempool activation: \
                mempool should not activate while syncing lots of blocks",
            ),
            MempoolBehavior::ForceActivationAt(_) => unreachable!("checked by outer if condition"),
            _ => None,
        };

        if let Some(error) = mempool_check {
            // if the mempool does not behave as expected, we panic and kill the test process.
            // but we also need to kill the `zebrad` child before the test panics.
            child.kill()?;
            panic!("{error}")
        }
    }

    // make sure the child process is dead
    // if it has already exited, ignore that error
    let _ = child.kill();

    Ok(child.dir)
}

fn cached_mandatory_checkpoint_test_config() -> Result<ZebradConfig> {
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
/// Callers can supply an extra `test_child_predicate`, which is called on
/// the `TestChild` between the startup checks, and the final
/// `STOP_AT_HEIGHT_REGEX` check.
///
/// The `TestChild` is spawned with a timeout, so the predicate should use
/// `expect_stdout_line_matches` or `expect_stderr_line_matches`.
///
/// This test ignores the `ZEBRA_SKIP_NETWORK_TESTS` env var.
///
/// # Test Status
///
/// Returns an error if the child exits or the fixed timeout elapses
/// before `STOP_AT_HEIGHT_REGEX` is found.
fn create_cached_database_height<P>(
    network: Network,
    height: Height,
    debug_skip_parameter_preload: bool,
    checkpoint_sync: bool,
    test_child_predicate: impl Into<Option<P>>,
) -> Result<()>
where
    P: FnOnce(&mut TestChild<PathBuf>) -> Result<()>,
{
    println!("Creating cached database");
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
        .spawn_child(&["start"])?
        .with_timeout(timeout)
        .bypass_test_capture(true);

    let network = format!("network: {},", network);
    child.expect_stdout_line_matches(&network)?;

    child.expect_stdout_line_matches("starting legacy chain check")?;
    child.expect_stdout_line_matches("no legacy chain found")?;

    if let Some(test_child_predicate) = test_child_predicate.into() {
        test_child_predicate(&mut child)?;
    }

    child.expect_stdout_line_matches(STOP_AT_HEIGHT_REGEX)?;

    child.kill()?;

    Ok(())
}

fn create_cached_database(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height();
    create_cached_database_height(
        network,
        height,
        true,
        // Use checkpoints to increase sync performance while caching the database
        true,
        |test_child: &mut TestChild<PathBuf>| {
            // make sure pre-cached databases finish before the mandatory checkpoint
            //
            // TODO: this check passes even if we reach the mandatory checkpoint,
            //       because we sync finalized state, then non-finalized state.
            //       Instead, fail if we see "best non-finalized chain root" in the logs.
            test_child.expect_stdout_line_matches("CommitFinalized request")?;
            Ok(())
        },
    )
}

fn sync_past_mandatory_checkpoint(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height() + 1200;
    create_cached_database_height(
        network,
        height.unwrap(),
        false,
        // Test full validation by turning checkpoints off
        false,
        |test_child: &mut TestChild<PathBuf>| {
            // make sure cached database tests finish after the mandatory checkpoint,
            // using the non-finalized state (the checkpoint_sync config must be false)
            test_child.expect_stdout_line_matches("best non-finalized chain root")?;
            Ok(())
        },
    )
}

// These tests are ignored because they're too long running to run during our
// traditional CI, and they depend on persistent state that cannot be made
// available in github actions or google cloud build. Instead we run these tests
// directly in a vm we spin up on google compute engine, where we can mount
// drives populated by the first two tests, snapshot those drives, and then use
// those to more quickly run the second two tests.

/// Sync up to the mandatory checkpoint height on mainnet and stop.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_mainnet", test)]
fn sync_to_mandatory_checkpoint_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    create_cached_database(network).unwrap();
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_testnet", test)]
fn sync_to_mandatory_checkpoint_testnet() {
    zebra_test::init();
    let network = Testnet;
    create_cached_database(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_mainnet", test)]
fn sync_past_mandatory_checkpoint_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    sync_past_mandatory_checkpoint(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on testnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_testnet", test)]
fn sync_past_mandatory_checkpoint_testnet() {
    zebra_test::init();
    let network = Testnet;
    sync_past_mandatory_checkpoint(network).unwrap();
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

    let dir = testdir()?.with_config(&mut config)?;
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
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.any_output_line_contains(
        "# TYPE zebrad_build_info counter",
        &body,
        "metrics exporter response",
        "the metrics response header",
    )?;
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in metrics exporter response");

    // Make sure metrics was started
    output.stdout_line_contains(format!("Opened metrics endpoint at {}", endpoint).as_str())?;

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

    let dir = testdir()?.with_config(&mut config)?;
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

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_line_contains(format!("Opened tracing endpoint at {}", endpoint).as_str())?;
    // TODO: Match some trace level messages from output

    // Make sure the endpoint header is correct
    // The header is split over two lines. But we don't want to require line
    // breaks at a specific word, so we run two checks for different substrings.
    output.any_output_line_contains(
        "HTTP endpoint allows dynamic control of the filter",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    output.any_output_line_contains(
        "tracing events",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in tracing filter response");

    // Make sure endpoint requests change the filter
    output.any_output_line_contains(
        "zebrad=debug",
        &tracing_body,
        "tracing filter endpoint response",
        "the modified tracing filter",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in modified tracing filter response");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

// TODO: RPC endpoint and port conflict tests (#3165)

/// Launch `zebrad` with an RPC port, and make sure `lightwalletd` works with Zebra.
///
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_integration() -> Result<()> {
    zebra_test::init();

    // Skip the test unless we specifically asked for it
    //
    // TODO: check if the lightwalletd binary is in the PATH?
    //       (this doesn't seem to be implemented in the standard library)
    if env::var("ZEBRA_TEST_LIGHTWALLETD").is_err() {
        tracing::info!(
            "skipped lightwalletd integration test, \
             set the 'ZEBRA_TEST_LIGHTWALLETD' environmental variable to run the test",
        );

        return Ok(());
    }

    // Launch zebrad

    // [Note on port conflict](#Note on port conflict)
    let listen_port = random_known_port();
    let listen_ip = "127.0.0.1".parse().expect("hard-coded IP is valid");
    let zebra_rpc_listener = SocketAddr::new(listen_ip, listen_port);

    // Write a configuration that has the rpc listen_addr option set
    // TODO: split this config into another function?
    let mut config = default_test_config()?;
    config.rpc.listen_addr = Some(zebra_rpc_listener);

    let zdir = testdir()?.with_config(&mut config)?;
    let mut zebrad = zdir.spawn_child(&["start"])?.with_timeout(LAUNCH_DELAY);

    // Wait until `zebrad` has opened the RPC endpoint
    zebrad.expect_stdout_line_matches(
        format!("Opened RPC endpoint at {}", zebra_rpc_listener).as_str(),
    )?;

    // Launch lightwalletd

    // Write a fake zcashd configuration that has the rpcbind and rpcport options set
    let ldir = testdir()?;
    let ldir = ldir.with_lightwalletd_config(zebra_rpc_listener)?;

    // Launch the lightwalletd process
    let result = ldir.spawn_lightwalletd_child(&[]);
    let (lightwalletd, zebrad) = zebrad.kill_on_error(result)?;
    let mut lightwalletd = lightwalletd.with_timeout(LAUNCH_DELAY);

    // Wait until `lightwalletd` has launched
    let result = lightwalletd.expect_stdout_line_matches("Starting gRPC server");
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // Check that `lightwalletd` is calling the expected Zebra RPCs
    //
    // TODO: add extra checks when we add new Zebra RPCs

    // get_blockchain_info
    let result = lightwalletd.expect_stdout_line_matches("Got sapling height");
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    let result = lightwalletd.expect_stdout_line_matches("Found 0 blocks in cache");
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // Check that `lightwalletd` got to the first unimplemented Zebra RPC
    //
    // TODO: update the missing method name when we add a new Zebra RPC

    let result = lightwalletd
        .expect_stdout_line_matches("Method not found.*error zcashd getbestblockhash rpc");
    let (_, zebrad) = zebrad.kill_on_error(result)?;
    let result = lightwalletd.expect_stdout_line_matches(
        "Lightwalletd died with a Fatal error. Check logfile for details",
    );
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // Cleanup both processes

    let result = lightwalletd.kill();
    let (_, mut zebrad) = zebrad.kill_on_error(result)?;
    zebrad.kill()?;

    let lightwalletd_output = lightwalletd.wait_with_output()?.assert_failure()?;
    let zebrad_output = zebrad.wait_with_output()?.assert_failure()?;

    // If the test fails here, see the [note on port conflict](#Note on port conflict)
    //
    // TODO: change lightwalletd to `assert_was_killed` when enough RPCs are implemented
    lightwalletd_output
        .assert_was_not_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    zebrad_output
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
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(
        "Opened Zcash protocol endpoint at {}",
        listen_addr
    ));

    // From another folder create a configuration with the same listener.
    // `network.listen_addr` will be the same in the 2 nodes.
    // (But since the config is ephemeral, they will have different state paths.)
    let dir2 = testdir()?.with_config(&mut config)?;

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
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened metrics endpoint at {}", listen_addr));

    // From another folder create a configuration with the same endpoint.
    // `metrics.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

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
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened tracing endpoint at {}", listen_addr));

    // From another folder create a configuration with the same endpoint.
    // `tracing.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

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
    let dir_conflict = testdir()?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let contains = if cfg!(unix) {
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
        regex::escape(&contains).as_str(),
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
    let mut node1 = first_dir.spawn_child(&["start"])?;

    // Wait until node1 has used the conflicting resource.
    node1.expect_stdout_line_matches(first_stdout_regex)?;

    // Spawn the second node
    let node2 = second_dir.spawn_child(&["start"]);
    let (node2, mut node1) = node1.kill_on_error(node2)?;

    // Wait a few seconds and kill first node.
    // Second node is terminated by panic, no need to kill.
    std::thread::sleep(LAUNCH_DELAY);
    let node1_kill_res = node1.kill();
    let (_, mut node2) = node2.kill_on_error(node1_kill_res)?;

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
            .context_from(&mut node1)
            .map(|_| ());
    }

    // Now we're sure both nodes are dead, and we have both their outputs
    let output1 = node1.wait_with_output().context_from(&mut node2)?;
    let output2 = node2.wait_with_output().context_from(&output1)?;

    // Make sure the first node was killed, rather than exiting with an error.
    output1
        .assert_was_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output2)?;

    // Make sure node2 has the expected resource conflict.
    output2
        .stderr_line_matches(second_stderr_regex)
        .context_from(&output1)?;
    output2
        .assert_was_not_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output1)?;

    Ok(())
}

/// What the expected behavior of the mempool is for a test that uses [`sync_until`].
enum MempoolBehavior {
    /// The mempool should be forced to activate at a certain height, for debug purposes.
    ForceActivationAt(Height),

    /// The mempool should be automatically activated.
    ShouldAutomaticallyActivate,

    /// The mempool should not become active during the test.
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

    /// Returns `true` if the mempool should be forcefully activated at a specified height.
    pub fn is_forced_activation(&self) -> bool {
        matches!(self, MempoolBehavior::ForceActivationAt(_))
    }
}
