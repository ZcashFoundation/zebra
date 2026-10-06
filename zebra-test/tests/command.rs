//! Tests for the [`zebra_test::command`] module.

use std::{
    process::Command,
    time::{Duration, Instant},
};

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
            eprintln!("Skipping test because '{cmd} {args:?}' returned error {e:?}");
            false
        }
        Ok(status) if !status.success() => {
            eprintln!("Skipping test because '{cmd} {args:?}' returned status {status:?}");
            false
        }
        _ => true,
    }
}

/// Test if a process that keeps on producing lines of output is killed after the timeout.
#[test]
fn kill_on_timeout_output_continuous_lines() -> Result<()> {
    let _init_guard = zebra_test::init();

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
        .spawn_child_with_command(TEST_CMD, args!["-v", "-n", "1024", "/dev/zero"])?
        .with_timeout(Duration::from_secs(2));

    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Test if the tests pass for a process that produces a single line of output,
/// then exits before the timeout.
#[test]
fn finish_before_timeout_output_single_line() -> Result<()> {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return Ok(());
    }

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Test if tests pass for a process that produces a small amount of output,
/// with no newlines, then exits before the timeout.
#[test]
fn finish_before_timeout_short_output_no_newlines() -> Result<()> {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "printf";
    // Skip the test if the test system does not have the command
    // The empty argument is required, because printf expects at least one argument.
    if !is_command_available(TEST_CMD, &[""]) {
        return Ok(());
    }

    let mut child = tempdir()?
        .spawn_child_with_command(TEST_CMD, args!["zebra_test_output"])?
        .with_timeout(Duration::from_secs(2));

    // We use a non-matching regex, to trigger the timeout.
    assert!(child
        .expect_stdout_line_matches("this regex should not match")
        .is_err());

    Ok(())
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// and fail with a test failure message.
#[test]
fn failure_regex_matches_stdout_failure_message() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    let expected_error = child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();

    let expected_error = format!("{expected_error:?}");
    assert!(
        expected_error.contains("Logged a failure message"),
        "error did not contain expected failure message: {expected_error}",
    );
}

/// Make sure failure regexes detect when a child process prints a failure message to stderr,
/// and panic with a test failure message.
#[test]
fn failure_regex_matches_stderr_failure_message() {
    let _init_guard = zebra_test::init();

    // The read command prints its prompt to stderr.
    //
    // This is tricky to get right, because:
    // - some read command versions only accept integer timeouts
    // - some installs only have read as a shell builtin
    // - some `sh` shells don't allow the `-t` option for read
    const TEST_CMD: &str = "bash";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["-c", "read -t 1 -p failure_message"]) {
        return;
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args![ "-c": "read -t 1 -p failure_message" ])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    let expected_error = child
        .expect_stderr_line_matches("this regex should not match")
        .unwrap_err();

    let expected_error = format!("{expected_error:?}");
    assert!(
        expected_error.contains("Logged a failure message"),
        "error did not contain expected failure message: {expected_error}",
    );
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is dropped without being killed.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_drop() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
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

/// When checking output, make sure failure regexes detect when a child process
/// prints a failure message to stdout, then they fail the test,
/// and read any extra multi-line output from the child process.
#[test]
fn failure_regex_reads_multi_line_output_on_expect_line() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(
            TEST_CMD,
            args![
                "failure_message\n\
                 multi-line failure message"
            ],
        )
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("failure_message", RegexSet::empty());

    // Any method that reads output should work here.
    // We use a non-matching regex, to trigger the failure panic.
    let expected_error = child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();

    let expected_error = format!("{expected_error:?}");
    assert!(expected_error.contains("failure_message"));
    assert!(expected_error.contains("multi-line failure message"));
}

/// On drop, make sure failure regexes detect when a child process prints a failure message.
/// then they fail the test, and read any extra multi-line output from the child process.
#[test]
fn failure_regex_reads_multi_line_output_on_drop() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let panic = std::panic::catch_unwind(|| {
        let _child = tempdir()
            .unwrap()
            .spawn_child_with_command(
                TEST_CMD,
                args![
                    "failure_message\n\
                 multi-line failure message"
                ],
            )
            .unwrap()
            .with_timeout(Duration::from_secs(5))
            .with_failure_regex_set("failure_message", RegexSet::empty());

        // Give the child process enough time to print its output.
        std::thread::sleep(Duration::from_secs(1));

        // Drop should read all unread output.
    })
    .expect_err("failure regex must reject the child's output on drop");
    let diagnostic = panic
        .downcast_ref::<String>()
        .expect("Drop formats its diagnostic as a String");
    assert!(diagnostic.contains("failure_message"));
    assert!(diagnostic.contains("multi-line failure message"));
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is killed.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_kill() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
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
    child.kill(true).unwrap();
}

/// Make sure failure regexes detect when a child process prints a failure message to stdout,
/// then the child process is killed on error.
#[test]
#[should_panic(expected = "Logged a failure message")]
fn failure_regex_matches_stdout_failure_message_kill_on_error() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
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
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
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
#[test]
fn failure_regex_timeout_continuous_output() {
    let _init_guard = zebra_test::init();

    // Ideally, we'd want to use the 'yes' command here, but BSD yes treats
    // every string as an argument to repeat - so we can't test if it is
    // present on the system.
    const TEST_CMD: &str = "hexdump";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &["/dev/null"]) {
        return;
    }

    // Without '-v', hexdump hides duplicate lines. But we want duplicate lines
    // in this test.
    let mut child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["-v", "/dev/zero"])
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .with_failure_regex_set("0", RegexSet::empty());

    // We use a non-matching regex, to trigger the timeout and the failure panic.
    let expected_error = child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();

    let expected_error = format!("{expected_error:?}");
    assert!(
        expected_error.contains("Logged a failure message"),
        "error did not contain expected failure message: {expected_error}",
    );
}

/// Make sure failure regexes are checked when a child process prints a failure message to stdout,
/// then the child process' output is waited for.
///
/// This is an error, but we still want to check failure logs.
#[test]
fn failure_regex_matches_stdout_failure_message_wait_for_output() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
    }

    let child = tempdir()
        .unwrap()
        .spawn_child_with_command(TEST_CMD, args!["failure_message"])
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .with_failure_regex_set("fail", RegexSet::empty());

    // Give the child process enough time to print its output.
    std::thread::sleep(Duration::from_secs(1));

    let error = child.wait_with_output().unwrap_err();
    assert!(format!("{error:?}").contains("Logged a failure message"));
}

/// Make sure failure regex iters detect when a child process prints a failure message to stdout,
/// and panic with a test failure message.
#[test]
fn failure_regex_iter_matches_stdout_failure_message() {
    let _init_guard = zebra_test::init();

    const TEST_CMD: &str = "echo";
    // Skip the test if the test system does not have the command
    if !is_command_available(TEST_CMD, &[]) {
        return;
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
    let expected_error = child
        .expect_stdout_line_matches("this regex should not match")
        .unwrap_err();

    let expected_error = format!("{expected_error:?}");
    assert!(
        expected_error.contains("Logged a failure message"),
        "error did not contain expected failure message: {expected_error}",
    );
}

/// Make sure ignore regexes override failure regexes.
#[test]
fn ignore_regex_ignores_stdout_failure_message() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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

#[cfg(unix)]
fn shell_child(script: &str) -> Result<zebra_test::command::TestChild<tempfile::TempDir>> {
    tempdir()?.spawn_child_with_command("sh", args!["-c": script])
}

/// Quiet pipes and incomplete lines on either stream must not defeat the child deadline.
#[cfg(unix)]
#[test]
fn child_deadline_quiet_and_partial_lines() -> Result<()> {
    let _init_guard = zebra_test::init();
    for stderr in [false, true] {
        for partial in [false, true] {
            let script = match (stderr, partial) {
                (_, false) => "exec sleep 120",
                (false, true) => "printf partial; exec sleep 120",
                (true, true) => "printf partial >&2; exec sleep 120",
            };
            let mut child = shell_child(script)?.with_timeout(Duration::from_secs(1));
            let start = Instant::now();
            let error = if stderr {
                child.expect_stderr_line_matches("never")
            } else {
                child.expect_stdout_line_matches("never")
            }
            .unwrap_err();
            assert!(start.elapsed() < Duration::from_secs(5));
            assert!(!child.is_running(), "deadline must kill and reap the child");
            if partial {
                assert!(format!("{error:?}").contains("partial"));
            }
        }
    }
    Ok(())
}

/// The non-matching line-reader APIs must also interrupt quiet pipes and clean up before panic.
#[cfg(unix)]
#[test]
fn child_deadline_wait_for_line() -> Result<()> {
    let _init_guard = zebra_test::init();
    for stderr in [false, true] {
        let mut child = shell_child("exec sleep 120")?.with_timeout(Duration::from_secs(1));
        let start = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if stderr {
                child.wait_for_stderr_line(None)
            } else {
                child.wait_for_stdout_line(None)
            }
        }));
        assert!(result.is_err(), "deadline is an error, not end-of-stream");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!child.is_running());
    }
    Ok(())
}

/// A ready log is not a successful exit; the same deadline must still bound the process wait.
#[cfg(unix)]
#[test]
fn child_deadline_wait_for_exit() -> Result<()> {
    let _init_guard = zebra_test::init();
    let mut child =
        shell_child("printf 'ready\\n'; exec sleep 120")?.with_timeout(Duration::from_secs(1));
    child.expect_stdout_line_matches("^ready$")?;
    let start = Instant::now();
    let error = child.wait_with_output().unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(format!("{error:?}").contains("deadline"));
    Ok(())
}

/// Matchers take both pipe handles, but exit waiting must still drain more than pipe capacity.
#[cfg(unix)]
#[test]
fn child_output_drains_both_pipes_after_matches() -> Result<()> {
    let _init_guard = zebra_test::init();
    let mut child = shell_child(
        "printf 'ready\\n'; printf 'ready\\n' >&2; \
         i=0; while [ \"$i\" -lt 4096 ]; do \
         printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\\n'; \
         printf 'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\\n' >&2; \
         i=$((i + 1)); done",
    )?
    .with_timeout(Duration::from_secs(10));
    child.expect_stdout_line_matches("^ready$")?;
    child.expect_stderr_line_matches("^ready$")?;
    let output = child.wait_with_output()?.assert_success()?;
    output.assert_was_not_killed()?;
    assert_eq!(
        output.output.stdout,
        format!("{}\n", "0123456789abcdef".repeat(4))
            .repeat(4096)
            .as_bytes()
    );
    assert_eq!(
        output.output.stderr,
        format!("{}\n", "fedcba9876543210".repeat(4))
            .repeat(4096)
            .as_bytes()
    );
    Ok(())
}

/// Preserve exact raw bytes and the real exit code, including a normal unsuccessful exit.
#[cfg(unix)]
#[test]
fn child_output_preserves_natural_exit_and_raw_bytes() -> Result<()> {
    let _init_guard = zebra_test::init();
    for code in [0, 7] {
        let output = shell_child(&format!(
            "printf 'out\\377'; printf 'err\\376' >&2; exit {code}"
        ))?
        .with_timeout(Duration::from_secs(5))
        .wait_with_output()?;
        assert_eq!(output.output.status.code(), Some(code));
        assert_eq!(output.output.stdout, b"out\xff");
        assert_eq!(output.output.stderr, b"err\xfe");
        output.assert_was_not_killed()?;
    }
    Ok(())
}

/// Like `std::process::Child::wait_with_output`, waiting must deliver EOF to piped stdin.
#[cfg(unix)]
#[test]
fn child_output_closes_stdin_before_waiting() -> Result<()> {
    use std::io::Write as _;
    use zebra_test::command::CommandExt;

    let _init_guard = zebra_test::init();
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2(tempdir()?, "cat")?
        .with_timeout(Duration::from_secs(2));
    child
        .child
        .as_mut()
        .expect("spawn2 retains the child")
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"input until EOF")?;

    let output = child.wait_with_output()?.assert_success()?;
    assert_eq!(output.output.stdout, b"input until EOF");
    output.assert_was_not_killed()?;
    Ok(())
}

/// Changing the timeout or public deadline must update readers already used by a matcher.
#[cfg(unix)]
#[test]
fn child_deadline_can_be_reset_after_matching() -> Result<()> {
    let _init_guard = zebra_test::init();
    for direct in [false, true] {
        let mut child = shell_child("printf 'ready\\n'; sleep 1; printf 'done\\n'")?
            .with_timeout(Duration::from_millis(500));
        child.expect_stdout_line_matches("^ready$")?;
        if direct {
            child.deadline = Some(Instant::now() + Duration::from_secs(5));
        } else {
            child = child.with_timeout(Duration::from_secs(5));
        }
        assert_eq!(child.expect_stdout_line_matches("^done$")?, "done");
        child
            .wait_with_output()?
            .assert_success()?
            .assert_was_not_killed()?;
    }
    Ok(())
}

/// An expired startup deadline must not affect cleanup when the caller starts a new phase.
#[cfg(unix)]
#[test]
fn child_deadline_can_be_reset_before_cleanup() -> Result<()> {
    let _init_guard = zebra_test::init();
    let mut child =
        shell_child("printf 'ready\\n'; exec sleep 120")?.with_timeout(Duration::from_secs(5));
    child.expect_stdout_line_matches("^ready$")?;
    child.deadline = Some(Instant::now() - Duration::from_secs(1));
    child.kill(false)?;
    child
        .with_timeout(Duration::from_secs(5))
        .wait_with_output()?
        .assert_was_killed()?;
    Ok(())
}

/// Observation and intentional shutdown can outlive a bounded startup phase.
#[cfg(unix)]
#[test]
fn child_startup_deadline_preserves_long_observation_and_kill() -> Result<()> {
    use std::io::Write as _;
    use zebra_test::command::CommandExt;

    let _init_guard = zebra_test::init();
    let mut child = Command::new("sh")
        .args([
            "-c",
            "printf 'ready\\n'; read gate; printf 'observed\\ntail\\n'; \
             printf 'observation complete\\n' >&2; read stop",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2(tempdir()?, "sh")?
        .with_timeout(Duration::from_secs(5));
    let startup_deadline = child
        .deadline
        .expect("a timeout establishes the child deadline");
    child.expect_stdout_line_matches("^ready$")?;
    child = child.with_timeout(Duration::from_secs(15));

    // The stdin barrier holds the child alive while the startup budget expires.
    std::thread::sleep(
        startup_deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
    );
    child
        .child
        .as_mut()
        .expect("spawn2 retains the child")
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(b"observe\n")?;
    child.expect_stdout_line_matches("^observed$")?;
    child.expect_stderr_line_matches("^observation complete$")?;
    assert!(Instant::now() > startup_deadline);
    assert!(
        child.is_running(),
        "the child must await intentional shutdown"
    );

    child.kill(false)?;
    let output = child.wait_with_output()?.assert_failure()?;
    output.assert_was_killed()?;
    assert_eq!(output.output.stdout, b"tail\n");
    Ok(())
}

#[cfg(windows)]
fn quiet_windows_child(script: &str) -> Result<zebra_test::command::TestChild<tempfile::TempDir>> {
    let script = format!("[Console]::Out.WriteLine('ready'); {script}; Start-Sleep -Seconds 120");
    let mut child = tempdir()?
        .spawn_child_with_command(
            "powershell.exe",
            args!["-NoProfile", "-NonInteractive", "-Command": script],
        )?
        .with_timeout(Duration::from_secs(30));
    child.expect_stdout_line_matches("^ready$")?;
    Ok(child.with_timeout(Duration::from_secs(1)))
}

/// Windows pipe polling must interrupt quiet and partial output on either stream.
#[cfg(windows)]
#[test]
fn child_deadline_quiet_and_partial_lines_windows() -> Result<()> {
    let _init_guard = zebra_test::init();
    for stderr in [false, true] {
        for partial in [false, true] {
            let stream = if stderr { "Error" } else { "Out" };
            let script = if partial {
                format!("[Console]::{stream}.Write('partial'); [Console]::{stream}.Flush()")
            } else {
                "$null".to_owned()
            };
            let mut child = quiet_windows_child(&script)?;
            let start = Instant::now();
            let error = if stderr {
                child.expect_stderr_line_matches("never")
            } else {
                child.expect_stdout_line_matches("never")
            }
            .unwrap_err();
            assert!(start.elapsed() < Duration::from_secs(5));
            assert!(!child.is_running(), "deadline must kill and reap the child");
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind),
                Some(std::io::ErrorKind::TimedOut),
            );
        }
    }
    Ok(())
}

/// A Windows exit deadline must clean up the process even after its ready log was consumed.
#[cfg(windows)]
#[test]
#[allow(unsafe_code)]
fn child_deadline_wait_for_exit_windows() -> Result<()> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

    let _init_guard = zebra_test::init();
    let child = quiet_windows_child("$null")?;
    let process = child
        .child
        .as_ref()
        .expect("the spawned child remains owned until wait_with_output")
        .as_handle()
        .try_clone_to_owned()?;
    let start = Instant::now();
    assert!(child.wait_with_output().is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
    // SAFETY: the owned process handle remains valid, and a zero timeout never blocks.
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
        WAIT_OBJECT_0,
    );
    Ok(())
}
