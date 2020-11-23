//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! ### Note on port conflict
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
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::try_err)]
#![allow(clippy::unknown_clippy_lints)]

use color_eyre::eyre::Result;
use eyre::WrapErr;
use tempdir::TempDir;

use std::{env, fs, io::Write, path::Path, path::PathBuf, time::Duration};

use zebra_chain::{
    block::Height,
    parameters::{
        Network::{self, *},
        NetworkUpgrade,
    },
};
use zebra_test::{command::TestDirExt, prelude::*};
use zebrad::config::ZebradConfig;

fn default_test_config() -> Result<ZebradConfig> {
    let mut config = ZebradConfig::default();
    config.state = zebra_state::Config::ephemeral();
    config.network.listen_addr = "127.0.0.1:0".parse()?;

    Ok(config)
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
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Add the given config to the test directory and use it for all
    /// subsequently spawned processes.
    fn with_config(self, config: ZebradConfig) -> Result<Self>;

    /// Overwrite any existing config the test directory and use it for all
    /// subsequently spawned processes.
    fn replace_config(self, config: ZebradConfig) -> Result<Self>;
}

impl<T> ZebradTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let path = self.as_ref();
        let default_config_path = path.join("zebrad.toml");

        if default_config_path.exists() {
            let mut extra_args: Vec<_> = Vec::new();
            extra_args.push("-c");
            extra_args.push(
                default_config_path
                    .as_path()
                    .to_str()
                    .expect("Path is valid Unicode"),
            );
            extra_args.extend_from_slice(args);
            self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad"), &extra_args)
        } else {
            self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad"), args)
        }
    }

    fn with_config(self, mut config: ZebradConfig) -> Result<Self> {
        let dir = self.as_ref();

        if !config.state.ephemeral {
            let cache_dir = dir.join("state");
            fs::create_dir(&cache_dir)?;
            config.state.cache_dir = cache_dir;
        }

        fs::File::create(dir.join("zebrad.toml"))?
            .write_all(toml::to_string(&config)?.as_bytes())?;

        Ok(self)
    }

    fn replace_config(self, mut config: ZebradConfig) -> Result<Self> {
        let dir = self.as_ref();

        if !config.state.ephemeral {
            let cache_dir = dir.join("state");

            // Create dir, ignoring existing directories
            match fs::create_dir(&cache_dir) {
                Ok(_) => {}
                Err(e) if (e.kind() == std::io::ErrorKind::AlreadyExists) => {}
                Err(e) => Err(e)?,
            };

            config.state.cache_dir = cache_dir;
        }

        let config_file = dir.join("zebrad.toml");

        // Remove any existing config before writing a new one
        let _ = fs::remove_file(config_file.clone());
        fs::File::create(config_file)?.write_all(toml::to_string(&config)?.as_bytes())?;

        Ok(self)
    }
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let child = testdir()?
        .with_config(default_test_config()?)?
        .spawn_child(&["generate"])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

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

    // Check if the temp dir still exist
    assert_with_context!(testdir.path().exists(), &output);

    // Check if the file was created
    assert_with_context!(generated_config_path.exists(), &output);

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(default_test_config()?)?;

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
    let testdir = testdir()?.with_config(persistent_test_config()?)?;

    let mut child = testdir.spawn_child(&["-v", "start"])?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
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

    let testdir = testdir()?.with_config(default_test_config()?)?;
    let testdir = &testdir;

    // Any free argument is valid
    let mut child = testdir.spawn_child(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
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

    let testdir = testdir()?.with_config(persistent_test_config()?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(&["-v", "start"])?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    // Check that we have persistent rocksdb database
    let cache_dir = testdir.path().join("state");
    assert_with_context!(cache_dir.read_dir()?.count() > 0, &output);

    Ok(())
}

#[test]
fn ephemeral_mode() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(default_test_config()?)?;
    let testdir = &testdir;

    // Any free argument is valid
    let mut child = testdir.spawn_child(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(!cache_dir.exists(), &output);

    Ok(())
}

#[test]
fn misconfigured_ephemeral_mode() -> Result<()> {
    zebra_test::init();

    let dir = TempDir::new("zebrad_tests")?;
    let cache_dir = dir.path().join("state");
    fs::create_dir(&cache_dir)?;

    // Write a configuration that has both cache_dir and ephemeral options set
    let mut config = default_test_config()?;
    // Although cache_dir has a default value, we set it a new temp directory
    // to test that it is empty later.
    config.state.cache_dir = cache_dir.clone();

    fs::File::create(dir.path().join("zebrad.toml"))?
        .write_all(toml::to_string(&config)?.as_bytes())?;

    // Any free argument is valid
    let mut child = dir
        .with_config(config)?
        .spawn_child(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    // Check that ephemeral takes precedence over cache_dir
    assert_with_context!(
        cache_dir
            .read_dir()
            .expect("cache_dir should still exist")
            .count()
            == 0,
        &output
    );

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(default_test_config()?)?;

    let child = testdir.spawn_child(&[])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(default_test_config()?)?;

    let child = testdir.spawn_child(&["version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_matches(r"^zebrad [0-9].[0-9].[0-9]-[A-Za-z]*.[0-9]\n$")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(default_test_config()?)?;
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

    // Check if the file was created
    assert_with_context!(generated_config_path.exists(), &output);

    // Run command using temp dir and kill it at 1 second
    let mut child = testdir.spawn_child(&[command])?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(expected_output)?;

    // [Note on port conflict](#Note on port conflict)
    output.assert_was_killed().wrap_err("Possible port or cache conflict. Are there other acceptance test, zebrad, or zcashd processes running?")?;

    // Check if the temp dir still exists
    assert_with_context!(testdir.path().exists(), &output);

    // Check if the created config file still exists
    assert_with_context!(generated_config_path.exists(), &output);

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

/// Test if `zebrad` can sync some larger checkpoints on testnet.
///
/// This test does not run by default, see `sync_large_checkpoints_mainnet`
/// for details.
#[test]
#[ignore]
fn sync_large_checkpoints_testnet() -> Result<()> {
    sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        Testnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
    )
    .map(|_tempdir| ())
}

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
        return Ok(testdir()?);
    }

    // Use a persistent state, so we can handle large syncs
    let mut config = persistent_test_config()?;
    // TODO: add convenience methods?
    config.network.network = network;
    config.state.debug_stop_at_height = Some(height.0);

    let tempdir = if let Some(reuse_tempdir) = reuse_tempdir {
        reuse_tempdir.replace_config(config)?
    } else {
        testdir()?.with_config(config)?
    };

    let mut child = tempdir.spawn_child(&["start"])?.with_timeout(timeout);

    // TODO: is there a way to check for testnet or mainnet here?
    // For example: "network=Mainnet" or "network=Testnet"
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

    fs::File::create(dir.join("zebrad.toml"))?.write_all(toml::to_string(&config)?.as_bytes())?;

    let mut child = dir
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

#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use hyper::{Client, Uri};

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let endpoint = "127.0.0.1:50001";
    let url = "http://127.0.0.1:50001";

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config()?;
    config.metrics.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = TempDir::new("zebrad_tests")?;
    fs::File::create(dir.path().join("zebrad.toml"))?
        .write_all(toml::to_string(&config)?.as_bytes())?;

    let mut child = dir.spawn_child(&["start"])?;

    // Run the program for a second before testing the endpoint
    std::thread::sleep(Duration::from_secs(1));

    // Create an http client
    let client = Client::new();

    // Test metrics endpoint
    let res = client.get(Uri::from_static(url)).await?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await?;
    assert!(std::str::from_utf8(&body)
        .unwrap()
        .contains("metrics snapshot"));

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure metrics was started
    output.stdout_contains(format!(r"Initializing metrics endpoint at {}", endpoint).as_str())?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

#[tokio::test]
#[ignore]
async fn tracing_endpoint() -> Result<()> {
    use hyper::{Body, Client, Request, Uri};

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let endpoint = "127.0.0.1:50002";
    let url_default = "http://127.0.0.1:50002";
    let url_filter = "http://127.0.0.1:50002/filter";

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config()?;
    config.tracing.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = TempDir::new("zebrad_tests")?;
    fs::File::create(dir.path().join("zebrad.toml"))?
        .write_all(toml::to_string(&config)?.as_bytes())?;

    let mut child = dir.spawn_child(&["start"])?;

    // Run the program for a second before testing the endpoint
    std::thread::sleep(Duration::from_secs(1));

    // Create an http client
    let client = Client::new();

    // Test tracing endpoint
    let res = client.get(Uri::from_static(url_default)).await?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await?;
    assert!(std::str::from_utf8(&body).unwrap().contains(
        "This HTTP endpoint allows dynamic control of the filter applied to\ntracing events."
    ));

    // Set a filter and make sure it was changed
    let request = Request::post(url_filter)
        .body(Body::from("zebrad=debug"))
        .unwrap();
    let _post = client.request(request).await?;

    let tracing_res = client.get(Uri::from_static(url_filter)).await?;
    assert!(tracing_res.status().is_success());
    let tracing_body = hyper::body::to_bytes(tracing_res).await?;
    assert!(std::str::from_utf8(&tracing_body)
        .unwrap()
        .contains("zebrad=debug"));

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_contains(format!(r"Initializing tracing endpoint at {}", endpoint).as_str())?;
    // Todo: Match some trace level messages from output

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
