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

use color_eyre::eyre::Result;
use eyre::WrapErr;
use tempdir::TempDir;

use std::{borrow::Borrow, fs, io::Write, time::Duration};

use zebra_chain::parameters::Network::{self, *};
use zebra_test::{command::TestDirExt, prelude::*};
use zebrad::config::ZebradConfig;

fn default_test_config() -> Result<ZebradConfig> {
    let mut config = ZebradConfig::default();
    config.state = zebra_state::Config::ephemeral();
    config.state.memory_cache_bytes = 256000000;
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
    Self: Borrow<TempDir> + Sized,
{
    /// Spawn `zebrad` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Add the given config to the test directory and use it for all
    /// subsequently spawned processes.
    fn with_config(self, config: ZebradConfig) -> Result<Self>;
}

impl<T> ZebradTestDirExt for T
where
    Self: TestDirExt + Borrow<TempDir> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let tempdir = self.borrow();
        let default_config_path = tempdir.path().join("zebrad.toml");

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
        let dir = self.borrow().path();

        if !config.state.ephemeral {
            let cache_dir = dir.join("state");
            fs::create_dir(&cache_dir)?;
            config.state.cache_dir = cache_dir;
        }

        fs::File::create(dir.join("zebrad.toml"))?
            .write_all(toml::to_string(&config)?.as_bytes())?;

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
fn revhex_args() -> Result<()> {
    zebra_test::init();
    let testdir = testdir()?.with_config(default_test_config()?)?;

    // Valid
    let child = testdir.spawn_child(&["revhex", "33eeff55"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_equals("55ffee33\n")?;

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

    // Check that we have persistent sled database
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

#[test]
#[ignore]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_one_checkpoint(Mainnet)
}

#[test]
#[ignore]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_one_checkpoint(Testnet)
}

fn sync_one_checkpoint(network: Network) -> Result<()> {
    zebra_test::init();

    let mut config = persistent_test_config()?;
    // TODO: add a convenience method?
    config.network.network = network;

    let mut child = testdir()?
        .with_config(config)?
        .spawn_child(&["start"])?
        .with_timeout(Duration::from_secs(20));

    // TODO: is there a way to check for testnet or mainnet here?
    // For example: "network=Mainnet" or "network=Testnet"
    child.expect_stdout("verified checkpoint range")?;
    child.kill()?;

    Ok(())
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
