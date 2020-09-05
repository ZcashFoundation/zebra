//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::{borrow::Borrow, fs, io::Write, time::Duration};
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

fn tempdir(config_mode: ConfigMode) -> Result<TempDir> {
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

    Ok(dir)
}

trait GetChild
where
    Self: Borrow<TempDir> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>>;
}

impl<T> GetChild for T
where
    Self: Borrow<TempDir> + Sized,
{
    fn spawn_child(self, args: &[&str]) -> Result<TestChild<Self>> {
        let tempdir = self.borrow();
        let mut cmd = test_cmd(env!("CARGO_BIN_EXE_zebrad"), tempdir.path())?;

        let default_config_path = tempdir.path().join("zebrad.toml");

        if default_config_path.exists() {
            cmd.arg("-c").arg(default_config_path);
        }

        Ok(cmd
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2(self)
            .unwrap())
    }
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();
    let child = tempdir(ConfigMode::Ephemeral)?.spawn_child(&["generate"])?;

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
    let testdir = tempdir(ConfigMode::NoConfig)?;
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
    let testdir = tempdir(ConfigMode::Ephemeral)?;

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
    let testdir = tempdir(ConfigMode::NoConfig)?;
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
    let testdir = tempdir(ConfigMode::Ephemeral)?;

    // Valid
    let child = testdir.spawn_child(&["revhex", "33eeff55"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_equals("55ffee33\n")?;

    Ok(())
}

#[test]
fn seed_no_args() -> Result<()> {
    zebra_test::init();
    let testdir = tempdir(ConfigMode::Ephemeral)?;

    let mut child = testdir.spawn_child(&["-v", "seed"])?;

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
    let testdir = tempdir(ConfigMode::Ephemeral)?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(&["seed", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(&["seed", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unexpected free argument `start`
    let child = testdir.spawn_child(&["seed", "start"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    zebra_test::init();
    // start caches state, so run one of the start tests with persistent state
    let testdir = tempdir(ConfigMode::Persistent)?;

    let mut child = testdir.spawn_child(&["-v", "start"])?;

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
    let testdir = tempdir(ConfigMode::Ephemeral)?;
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
    let testdir = tempdir(ConfigMode::Persistent)?;
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
    let testdir = tempdir(ConfigMode::Ephemeral)?;
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
    let mut child = dir.spawn_child(&["start", "argument"])?;
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
    let testdir = tempdir(ConfigMode::Ephemeral)?;

    let child = testdir.spawn_child(&[])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();
    let testdir = tempdir(ConfigMode::Ephemeral)?;

    let child = testdir.spawn_child(&["version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_matches(r"^zebrad [0-9].[0-9].[0-9]-[A-Za-z]*.[0-9]\n$")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();
    let testdir = tempdir(ConfigMode::Ephemeral)?;
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
    valid_generated_config("seed", r"Starting zebrad in seed mode")?;

    Ok(())
}

fn valid_generated_config(command: &str, expected_output: &str) -> Result<()> {
    zebra_test::init();
    let testdir = tempdir(ConfigMode::NoConfig)?;
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

    // If the test child has a cache or port conflict with another test, or a
    // running zebrad or zcashd, then it will panic. But the acceptance tests
    // expect it to run until it is killed.
    //
    // If these conflicts cause test failures:
    //   - run the tests in an isolated environment,
    //   - run zebrad on a custom cache path and port,
    //   - run zcashd on a custom port.
    output.assert_was_killed().expect("Expected zebrad with generated config to succeed. Are there other acceptance test, zebrad, or zcashd processes running?");

    // Check if the temp dir still exists
    assert_with_context!(testdir.path().exists(), &output);

    // Check if the created config file still exists
    assert_with_context!(generated_config_path.exists(), &output);

    Ok(())
}
