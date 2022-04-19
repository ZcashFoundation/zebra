use std::{process::Command, time::Duration};

use color_eyre::eyre::{eyre, Result};
use regex::RegexSet;
use tempfile::tempdir;

use zebra_test::{
    args,
    command::{TestDirExt, NO_MATCHES_REGEX_ITER},
    prelude::Stdio,
};

/// Returns true if `cmd` with `args` runs successfully.
///
/// On failure, prints an error message to stderr.
/// (This message is captured by the test runner, use `cargo test -- --nocapture` to see it.)
///
/// The command's stdout and stderr are ignored.
#[allow(clippy::print_stderr)]
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
    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["-v", "/dev/zero"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Test if the tests pass for a process that produces a single line of output,
/// then exits before the timeout.
//
// TODO: create a similar test that pauses after output (#1140)
#[test]
fn finish_before_timeout_output_single_line() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return Ok(());
    }

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

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

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["/dev/zero"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Test if tests pass for a process that produces a small amount of output,
/// with no newlines, then exits before the timeout.
//
// TODO: create a similar test that pauses after output (#1140)
#[test]
fn finish_before_timeout_short_output_no_newlines() -> Result<()> {
    zebra_test::init();

    const TEST_CMD: &str = "printf";
    // Skip the test if the test system does not have the command
    // The empty argument is required, because printf expects at least one argument.
    if !is_command_available(TEST_CMD, &[""]) {
        return Ok(());
    }

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

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

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["120"])?
        .with_timeout(Duration::from_secs(2));

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// and panic with a test failure message.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();
}

/// Make sure failure regexes detect when a child process prints a failure message to stderr,
/// and panic with a test failure message.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stderr_failure_message() {
    zebra_test::init();

    // The read command prints its prompt to stderr.
    //
    // This is tricky to get right, because:
    // - some read command versions only accept integer timeouts
    // - some installs only have read as a shell builtin
    // - some `sh` shells don't allow the `-t` option for read
    const TEST_CMD: &str = "bash";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["-c", "read -t 1 -p failure_message"]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args![ "-c": "read -t 1 -p failure_message" ])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    child
        .expect_stderr_line_matches("this regex should not match")
        .unwrap_err();
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is dropped without being killed.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_drop() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let _child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    // Drop should read all unread output.
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is killed.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_kill() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    // Kill should read all unread output to generate the error context,
    // or the output should be read on drop.
    child.kill().unwrap();
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is killed on error.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_kill_on_error() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    // Kill on error should read all unread output to generate the error context,
    // or the output should be read on drop.
    let test_error: Result<()> = Err(eyre!("test error"));
    child.kill_on_error(test_error).unwrap();
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is not killed because there is no error.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_no_kill_on_error() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    // Kill on error should read all unread output to generate the error context,
    // or the output should be read on drop.
    let test_ok: Result<()> = Ok(());
    child.kill_on_error(test_ok).unwrap();
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then times out waiting for a specific output line.
///
/// TODO: test the failure regex on timeouts with no output (#1140)
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_timeout_continuous_output() {
    zebra_test::init();

    // Ideally, we'd want to use the 'yes' command here, but BSD yes treats
    // every string as an argument to repeat - so we can't test if it is
    // present on the system.
    const TEST_CMD: &str = "hexdump";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["/dev/null"]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    // Without '-v', hexdump hides duplicate lines. But we want duplicate lines
    // in this test.
    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["-v", "/dev/zero"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_set("0", RegexSet::empty());

    // We need to use expect_stdout_line_matches, because wait_with_output ignores timeouts.
    // We use a non-matching regex, to trigger the timeout and the failure panic.
    child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();
}

/// Make sure failure regexes are checked when a child process prints a failure message to stdout,
/// then the child process' output is waited for.
///
/// This is an error, but we still want to check failure logs.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_wait_for_output() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    // Wait with output should read all unread output to generate the error context,
    // or the output should be read on drop.
    child.wait_with_output().unwrap_err();
}

/// Make sure failure regex iters detect when a child process prints a failure message to stdout,
/// and panic with a test failure message.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_iter_matches_stdout_failure_message() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        panic!(
            "skipping test: command not available\n\
             fake panic message: Logged a failure message"
        );
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_iter(
            ["fail"].iter().cloned(),
            NO_MATCHES_REGEX_ITER.iter().cloned(),
        );

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();
}

/// Make sure ignore regexes override failure regexes.
#[test]
fn ignore_regex_ignores_stdout_failure_message() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message ignore_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_set("fail", "ignore");

    // Any method that reads output should work here.
    child.expect_stdout_line_matches("ignore_message").unwrap();
}

/// Make sure ignore regex iters override failure regex iters.
#[test]
fn ignore_regex_iter_ignores_stdout_failure_message() {
    zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message ignore_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_iter(["fail"].iter().cloned(), ["ignore"].iter().cloned());

    // Any method that reads output should work here.
    child.expect_stdout_line_matches("ignore_message").unwrap();
}
