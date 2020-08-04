//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::time::Duration;
use zebra_test::prelude::*;

/// The kill signal exit status on unix platforms.
#[cfg(unix)]
const KILL_STATUS: i32 = 9;
/// The kill exit code on non-unix platforms.
#[cfg(not(unix))]
const KILL_STATUS: i32 = 1;

// Todo: The following 3 helper functions can probably be abstracted into one
pub fn get_child_single_arg(arg: &str) -> Result<(zebra_test::command::TestChild, impl Drop)> {
    let (mut cmd, guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    Ok((
        cmd.arg(arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        guard,
    ))
}

pub fn get_child_multi_args(args: &[&str]) -> Result<(zebra_test::command::TestChild, impl Drop)> {
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

pub fn get_child_no_args() -> Result<(zebra_test::command::TestChild, impl Drop)> {
    let (mut cmd, guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    Ok((
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        guard,
    ))
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child_single_arg("generate")?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"# Default configuration for zebrad.")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _guard) = get_child_multi_args(&["generate", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child_multi_args(&["generate", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let (child, _guard) = get_child_multi_args(&["generate", "-o"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // Valid
    let (child, _guard) = get_child_multi_args(&["generate", "-o", "file.yaml"])?;
    let output = child.wait_with_output()?;
    output.assert_success()?;

    // Todo: Check if the file was created

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child_single_arg("help")?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

    // The subcommand "argument" wasn't recognized.
    let (child, _guard) = get_child_multi_args(&["help", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let (child, _guard) = get_child_multi_args(&["help", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn revhex_args() -> Result<()> {
    zebra_test::init();

    // Valid
    let (child, _guard) = get_child_multi_args(&["revhex", "33eeff55"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"55ffee33")?;

    Ok(())
}

fn seed_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _guard) = get_child_single_arg("seed")?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad in seed mode")?;

    // Make sure the command was killed
    assert_eq!(output.exit_status(), Some(KILL_STATUS));

    Ok(())
}

#[test]
fn seed_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _guard) = get_child_multi_args(&["seed", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child_multi_args(&["seed", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unexpected free argument `start`
    let (child, _guard) = get_child_multi_args(&["seed", "start"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

fn start_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _guard) = get_child_single_arg("start")?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad")?;

    // Make sure the command was killed
    assert_eq!(output.exit_status(), Some(KILL_STATUS));

    Ok(())
}

fn start_args() -> Result<()> {
    zebra_test::init();

    // Any free argument is valid
    let (mut child, _guard) = get_child_multi_args(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    assert_eq!(output.exit_status(), Some(KILL_STATUS));

    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child_multi_args(&["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child_no_args()?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let (child, _guard) = get_child_single_arg("version")?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"zebrad [0-9].[0-9].[0-9]")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    // unexpected free argument `argument`
    let (child, _guard) = get_child_multi_args(&["version", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let (child, _guard) = get_child_multi_args(&["version", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn serialized_tests() -> Result<()> {
    start_no_args()?;
    start_args()?;
    seed_no_args()?;

    Ok(())
}
