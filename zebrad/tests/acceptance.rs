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
use std::{fs, io::Write, path::PathBuf, time::Duration};
use tempdir::TempDir;

use zebra_test::prelude::*;
use zebrad::config::ZebradConfig;

fn default_test_config() -> Result<ZebradConfig> {
    let mut config = ZebradConfig::default();
    config.state = zebra_state::Config::ephemeral();
    config.state.memory_cache_bytes = 256000000;
    config.network.listen_addr = "127.0.0.1:0".parse()?;

    Ok(config)
}

#[derive(PartialEq)]
enum ConfigMode {
    NoConfig,
    Ephemeral,
    Persistent,
}

fn tempdir(config_mode: ConfigMode) -> Result<(PathBuf, impl Drop)> {
    let dir = TempDir::new("zebrad_tests")?;

    if config_mode != ConfigMode::NoConfig {
        let mut config = default_test_config()?;
        if config_mode == ConfigMode::Persistent {
            let cache_dir = dir.path().join("state");
            fs::create_dir(&cache_dir)?;
            config.state.cache_dir = cache_dir;
            config.state.ephemeral = false;
        }

        fs::File::create(dir.path().join("zebrad.toml"))?
            .write_all(toml::to_string(&config)?.as_bytes())?;
    }

    Ok((dir.path().to_path_buf(), dir))
}

fn get_child(args: &[&str], tempdir: &PathBuf) -> Result<TestChild> {
    let mut cmd = test_cmd(env!("CARGO_BIN_EXE_zebrad"), &tempdir)?;

    let default_config_path = tempdir.join("zebrad.toml");
    if default_config_path.exists() {
        cmd.args(&["-c", default_config_path.to_str().unwrap()]);
    }
    Ok(cmd
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2()
        .unwrap())
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    let child = get_child(&["generate"], &tempdir)?;
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
    let (tempdir, _guard) = tempdir(ConfigMode::NoConfig)?;

    // unexpected free argument `argument`
    let child = get_child(&["generate", "argument"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = get_child(&["generate", "-f"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let child = get_child(&["generate", "-o"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // Add a config file name to tempdir path
    let mut generated_config_path = tempdir.clone();
    generated_config_path.push("zebrad.toml");

    // Valid
    let child = get_child(
        &["generate", "-o", generated_config_path.to_str().unwrap()],
        &tempdir,
    )?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // Check if the temp dir still exist
    assert_with_context!(tempdir.exists(), &output);

    // Check if the file was created
    assert_with_context!(generated_config_path.exists(), &output);

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    let child = get_child(&["help"], &tempdir)?;
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
    let (tempdir, _guard) = tempdir(ConfigMode::NoConfig)?;

    // The subcommand "argument" wasn't recognized.
    let child = get_child(&["help", "argument"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let child = get_child(&["help", "-f"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn revhex_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    // Valid
    let child = get_child(&["revhex", "33eeff55"], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_equals("55ffee33\n")?;

    Ok(())
}

#[test]
fn seed_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    let mut child = get_child(&["-v", "seed"], &tempdir)?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad in seed mode")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

#[test]
fn seed_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    // unexpected free argument `argument`
    let child = get_child(&["seed", "argument"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = get_child(&["seed", "-f"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unexpected free argument `start`
    let child = get_child(&["seed", "start"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    zebra_test::init();
    // start caches state, so run one of the start tests with persistent state
    let (tempdir, _guard) = tempdir(ConfigMode::Persistent)?;

    let mut child = get_child(&["-v", "start"], &tempdir)?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // start is the default mode, so we check for end of line, to distinguish it
    // from seed
    output.stdout_contains(r"Starting zebrad$")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    // Any free argument is valid
    let mut child = get_child(&["start", "argument"], &tempdir)?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    // unrecognized option `-f`
    let child = get_child(&["start", "-f"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn persistent_mode() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Persistent)?;

    let mut child = get_child(&["-v", "start"], &tempdir)?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    // Check that we have persistent sled database
    let cache_dir = tempdir.join("state");
    assert_with_context!(cache_dir.read_dir()?.count() > 0, &output);

    Ok(())
}

#[test]
fn ephemeral_mode() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    // Any free argument is valid
    let mut child = get_child(&["start", "argument"], &tempdir)?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = tempdir.join("state");
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

    let tempdir = dir.path().to_path_buf();

    // Any free argument is valid
    let mut child = get_child(&["start", "argument"], &tempdir)?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    // Check that ephemeral takes precedence over cache_dir
    assert_with_context!(cache_dir.read_dir()?.count() == 0, &output);

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    let child = get_child(&[], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    let child = get_child(&["version"], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_matches(r"^zebrad [0-9].[0-9].[0-9]-[A-Za-z]*.[0-9]\n$")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::Ephemeral)?;

    // unexpected free argument `argument`
    let child = get_child(&["version", "argument"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = get_child(&["version", "-f"], &tempdir)?;
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
    valid_generated_config("seed", r"Starting zebrad in seed mode")?;

    Ok(())
}

fn valid_generated_config(command: &str, expected_output: &str) -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(ConfigMode::NoConfig)?;

    // Add a config file name to tempdir path
    let mut generated_config_path = tempdir.clone();
    generated_config_path.push("zebrad.toml");

    // Generate configuration in temp dir path
    let child = get_child(
        &["generate", "-o", generated_config_path.to_str().unwrap()],
        &tempdir,
    )?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // Check if the file was created
    assert_with_context!(generated_config_path.exists(), &output);

    // Run command using temp dir and kill it at 1 second
    let mut child = get_child(&[command], &tempdir)?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(expected_output)?;

    // [Note on port conflict](#Note on port conflict)
    output.assert_was_killed().wrap_err("Possible port or cache conflict. Are there other acceptance test, zebrad, or zcashd processes running?")?;

    // Check if the temp dir still exists
    assert_with_context!(tempdir.exists(), &output);

    // Check if the created config file still exists
    assert_with_context!(generated_config_path.exists(), &output);

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

    let tempdir = dir.path().to_path_buf();

    let mut child = get_child(&["start"], &tempdir)?;

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

    let tempdir = dir.path().to_path_buf();

    let mut child = get_child(&["start"], &tempdir)?;

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
