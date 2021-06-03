// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]

use std::{process::Command, time::Duration};

use color_eyre::eyre::Result;
use tempdir::TempDir;

use zebra_test::{command::TestDirExt, prelude::Stdio};

/// Returns true if `cmd` with `args` runs successfully.
///
/// On failure, prints an error message to stderr.
/// (This message is captured by the test runner, use `cargo test -- --nocapture` to see it.)
///
/// The command's stdout and stderr are ignored.
fn is_command_available(cmd: &str, args: &[&str]) -> bool {
    let status = Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Err(e) => {
            eprintln!(
                "Skipping test because '{} {:?}' returned error {:?}",
                cmd, args, e
            );
            false
        }
        Ok(status) if !status.success() => {
            eprintln!(
                "Skipping test because '{} {:?}' returned status {:?}",
                cmd, args, status
            );
            false
        }
        _ => true,
    }
}

/// Test if a process that keeps on producing lines of output is killed after the timeout.
#[test]
fn kill_on_timeout_output_continuous_lines() -> Result<()> {
    zebra_test::init();

    // Ideally, we'd want to use the 'yes' command here, but BSD yes treats
    // every string as an argument to repeat - so we can't test if it is
    // present on the system.
    const TEST_CMD: &str = "hexdump";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["/dev/null"]) {
        return Ok(());
    }

    // Without '-v', hexdump hides duplicate lines. But we want duplicate lines
    // in this test.
    let mut child = TempDir::new("zebra_test")?
        .spawn_child_with_command(TEST_CMD, &["-v", "/dev/zero"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child.expect_stdout("this regex should not match").is_err());

    Ok(())
}

/// Test if the tests pass for a process that produces a single line of output,
/// then exits before the timeout.
//
// TODO: create a similar test that pauses after output
#[test]
fn finish_before_timeout_output_single_line() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return Ok(());
    }

    let mut child = TempDir::new("zebra_test")?
        .spawn_child_with_command(TEST_CMD, &["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child.expect_stdout("this regex should not match").is_err());

    Ok(())
}

/// Test if a process that keeps on producing output, but doesn't produce any newlines,
/// is killed after the timeout.
///
/// This test fails due to bugs in TestDirExt, see #1140 for details.
//#[test]
//#[ignore]
#[allow(dead_code)]
fn kill_on_timeout_continuous_output_no_newlines() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "cat";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["/dev/null"]) {
        return Ok(());
    }

    let mut child = TempDir::new("zebra_test")?
        .spawn_child_with_command(TEST_CMD, &["/dev/zero"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child.expect_stdout("this regex should not match").is_err());

    Ok(())
}

/// Test if tests pass for a process that produces a small amount of output,
/// with no newlines, then exits before the timeout.
//
// TODO: create a similar test that pauses after output
#[test]
fn finish_before_timeout_short_output_no_newlines() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "printf";
    // Skip the test if the test system does not have the command
    // The empty argument is required, because printf expects at least one argument.
    if !is_command_available(TEST_CMD, &[""]) {
        return Ok(());
    }

    let mut child = TempDir::new("zebra_test")?
        .spawn_child_with_command(TEST_CMD, &["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child.expect_stdout("this regex should not match").is_err());

    Ok(())
}

/// Test if the timeout works for a process that produces no output.
///
/// This test fails due to bugs in TestDirExt, see #1140 for details.
// #[test]
// #[ignore]
#[allow(dead_code)]
fn kill_on_timeout_no_output() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "sleep";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["0"]) {
        return Ok(());
    }

    let mut child = TempDir::new("zebra_test")?
        .spawn_child_with_command(TEST_CMD, &["120"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child.expect_stdout("this regex should not match").is_err());

    Ok(())
}
