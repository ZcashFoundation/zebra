//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zebra_test::prelude::*;

pub fn get_child(args: &[&str]) -> Result<(zebra_test::command::TestChild, impl Drop)> {
    let (mut cmd, guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    Ok((
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        guard,
    ))
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child(&["generate"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _guard) = get_child(&["generate", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child(&["generate", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let (child, _guard) = get_child(&["generate", "-o"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // create config file path in the same location as binary
    let binary_path = Path::new(env!("CARGO_BIN_EXE_zebrad")).parent().unwrap();
    let mut path = PathBuf::from(binary_path);
    path.push("zebrad.toml");

    // Valid
    let (child, _guard) = get_child(&["generate", "-o", path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Check if the file was created
    assert_eq!(path.exists(), true);

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child(&["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

    // The subcommand "argument" wasn't recognized.
    let (child, _guard) = get_child(&["help", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let (child, _guard) = get_child(&["help", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn revhex_args() -> Result<()> {
    zebra_test::init();

    // Valid
    let (child, _guard) = get_child(&["revhex", "33eeff55"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"55ffee33")?;

    Ok(())
}

fn seed_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _guard) = get_child(&["-v", "seed"])?;

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
    let (child, _guard) = get_child(&["seed", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child(&["seed", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unexpected free argument `start`
    let (child, _guard) = get_child(&["seed", "start"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

fn start_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _guard) = get_child(&["-v", "start"])?;

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
    let (mut child, _guard) = get_child(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    assert!(output.was_killed());

    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child(&["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child(&[])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child(&["version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"zebrad [0-9].[0-9].[0-9]")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _guard) = get_child(&["version", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child(&["version", "-f"])?;
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

    // Generate configuration in binary parent path
    let binary_path = Path::new(env!("CARGO_BIN_EXE_zebrad")).parent().unwrap();
    let mut path = PathBuf::from(binary_path);
    path.push("zebrad.toml");
    let (child, _guard) = get_child(&["generate", "-o", path.to_str().unwrap()])?;
    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Check if the file was created
    assert_eq!(path.exists(), true);

    // Check if it was created in the same path as binary
    assert_eq!(path.parent(), Some(binary_path));

    // Run start and kill it at 1 second
    let (mut child, _guard) = get_child(&["-c", path.to_str().unwrap(), "start"])?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    // Run seed program and kill it at 1 second
    let (mut child, _guard) = get_child(&["-c", path.to_str().unwrap(), "seed"])?;
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad in seed mode")?;

    // Make sure the command was killed
    assert!(output.was_killed());

    Ok(())
}
