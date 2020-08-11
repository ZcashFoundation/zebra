//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::path::PathBuf;
use std::time::Duration;
use zebra_test::prelude::*;

pub fn get_child(args: &[&str], tempdir_path: Option<&str>) -> Result<(TestChild, ZebraTestDir)> {
    let (mut cmd, tempdir) = test_cmd(env!("CARGO_BIN_EXE_zebrad"), tempdir_path)?;

    Ok((
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        tempdir,
    ))
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _tempdir) = get_child(&["generate"], None)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _tempdir) = get_child(&["generate", "argument"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _tempdir) = get_child(&["generate", "-f"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let (child, tempdir) = get_child(&["generate", "-o"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // create config file in tempdir
    let mut tempdir = PathBuf::from(&tempdir.path());
    tempdir.push("zebrad.toml");

    // Valid
    let (child, _tempdir) = get_child(&["generate", "-o", tempdir.to_str().unwrap()], None)?;

    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Check if the file was created
    assert_eq!(tempdir.exists(), true);

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _tempdir) = get_child(&["help"], None)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

    // The subcommand "argument" wasn't recognized.
    let (child, _tempdir) = get_child(&["help", "argument"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let (child, _tempdir) = get_child(&["help", "-f"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn revhex_args() -> Result<()> {
    zebra_test::init();

    // Valid
    let (child, _tempdir) = get_child(&["revhex", "33eeff55"], None)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"55ffee33")?;

    Ok(())
}

fn seed_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _tempdir) = get_child(&["-v", "seed"], None)?;

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

    // unexpected free argument `argument`
    let (child, _tempdir) = get_child(&["seed", "argument"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _tempdir) = get_child(&["seed", "-f"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unexpected free argument `start`
    let (child, _tempdir) = get_child(&["seed", "start"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

fn start_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _tempdir) = get_child(&["-v", "start"], None)?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    Ok(())
}

fn start_args() -> Result<()> {
    zebra_test::init();

    // Any free argument is valid
    let (mut child, _tempdir) = get_child(&["start", "argument"], None)?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    assert!(output.was_killed());

    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _tempdir) = get_child(&["start", "-f"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _tempdir) = get_child(&[], None)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _tempdir) = get_child(&["version"], None)?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"zebrad [0-9].[0-9].[0-9]")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _tempdir) = get_child(&["version", "argument"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _tempdir) = get_child(&["version", "-f"], None)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn serialized_tests() -> Result<()> {
    start_no_args()?;
    start_args()?;
    seed_no_args()?;
    valid_generated_config()?;

    Ok(())
}

fn valid_generated_config() -> Result<()> {
    zebra_test::init();

    // Get a temp dir
    let tempdir = ZebraTestDir::new("zebrad_tests");
    let mut path_config = PathBuf::from(tempdir.path());
    path_config.push("zebrad.toml");

    // Generate configuration in temp dir path
    let tempdir_string = path_config.parent().unwrap().to_str().unwrap();
    let config_string = path_config.to_str().unwrap();
    let (child, tempdir) = get_child(&["generate", "-o", config_string], None)?;

    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Check if the file was created
    assert_eq!(path_config.exists(), true);

    // Check if it was created in the tempdir
    assert_eq!(PathBuf::from(path_config.parent().unwrap()), tempdir.path());

    // Run start using temp dir and kill it at 1 second
    let (mut child, _tempdir) = get_child(&["-c", config_string, "start"], Some(tempdir_string))?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    // Run seed using temp dir and kill it at 1 second
    let (mut child, _tempdir) = get_child(&["-c", config_string, "seed"], Some(tempdir_string))?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad in seed mode")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    Ok(())
}
