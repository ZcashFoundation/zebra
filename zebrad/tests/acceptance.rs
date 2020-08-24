//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::{fs, io::Write, path::PathBuf, time::Duration};
use tempdir::TempDir;

use zebra_test::prelude::*;
use zebrad::config::ZebradConfig;

pub fn tempdir(create_config: bool) -> Result<(PathBuf, impl Drop)> {
    let dir = TempDir::new("zebrad_tests")?;

    if create_config {
        let cache_dir = dir.path().join("state");
        fs::create_dir(&cache_dir)?;

        let mut config = ZebradConfig::default();
        config.state.cache_dir = cache_dir;
        config.state.memory_cache_bytes = 256000000;
        config.network.listen_addr = "127.0.0.1:0".parse()?;

        fs::File::create(dir.path().join("zebrad.toml"))?
            .write_all(toml::to_string(&config)?.as_bytes())?;
    }

    Ok((dir.path().to_path_buf(), dir))
}

pub fn get_child(args: &[&str], tempdir: &PathBuf) -> Result<TestChild> {
    let mut cmd = test_cmd(env!("CARGO_BIN_EXE_zebrad"), &tempdir)?;

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
    let (tempdir, _guard) = tempdir(true)?;

    let child = get_child(&["generate"], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(false)?;

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
    output.assert_success()?;

    // Check if the temp dir still exist
    assert!(tempdir.exists());

    // Check if the file was created
    assert!(generated_config_path.exists());

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

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
    let (tempdir, _guard) = tempdir(true)?;

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
    let (tempdir, _guard) = tempdir(true)?;

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
    let (tempdir, _guard) = tempdir(true)?;

    let mut child = get_child(&["-v", "seed"], &tempdir)?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad in seed mode")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    Ok(())
}

#[test]
fn seed_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

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
    let (tempdir, _guard) = tempdir(true)?;

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
    assert!(output.was_killed());

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

    // Any free argument is valid
    let mut child = get_child(&["start", "argument"], &tempdir)?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    assert!(output.was_killed());

    output.assert_failure()?;

    // unrecognized option `-f`
    let child = get_child(&["start", "-f"], &tempdir)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

    let child = get_child(&[], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

    let child = get_child(&["version"], &tempdir)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_matches(r"^zebrad [0-9].[0-9].[0-9]-[A-Za-z]*.[0-9]\n$")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();
    let (tempdir, _guard) = tempdir(true)?;

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
    let (tempdir, _guard) = tempdir(false)?;

    // Add a config file name to tempdir path
    let mut generated_config_path = tempdir.clone();
    generated_config_path.push("zebrad.toml");

    // Generate configuration in temp dir path
    let child = get_child(
        &["generate", "-o", generated_config_path.to_str().unwrap()],
        &tempdir,
    )?;

    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Check if the file was created
    assert!(generated_config_path.exists());

    // Run command using temp dir and kill it at 1 second
    let mut child = get_child(
        &["-c", generated_config_path.to_str().unwrap(), command],
        &tempdir,
    )?;
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
    assert!(output.was_killed(), "Expected zebrad with generated config to succeed. Are there other acceptance test, zebrad, or zcashd processes running?");

    // Check if the temp dir still exists
    assert!(tempdir.exists());

    // Check if the created config file still exists
    assert!(generated_config_path.exists());

    Ok(())
}
