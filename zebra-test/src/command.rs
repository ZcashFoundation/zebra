//! Launching test commands for Zebra integration and acceptance tests.

use std::{
    convert::Infallible as NoDir,
    fmt::{self, Debug, Write as _},
    io::{BufRead, BufReader, Read, Write as _},
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use regex::RegexSet;
use tracing::instrument;

mod arguments;
pub mod to_regex;

pub use self::arguments::Arguments;
use self::to_regex::{CollectRegexSet, ToRegex, ToRegexSet};

/// A super-trait for [`Iterator`] + [`Debug`].
pub trait IteratorDebug: Iterator + Debug {}

impl<T> IteratorDebug for T where T: Iterator + Debug {}

/// Runs a command
pub fn test_cmd(command_path: &str, tempdir: &Path) -> Result<Command> {
    let mut cmd = Command::new(command_path);
    cmd.current_dir(tempdir);

    Ok(cmd)
}

// TODO: split these extensions into their own module

/// Wrappers for `Command` methods to integrate with [`zebra_test`].
pub trait CommandExt {
    /// wrapper for `status` fn on `Command` that constructs informative error
    /// reports
    fn status2(&mut self) -> Result<TestStatus, Report>;

    /// wrapper for `output` fn on `Command` that constructs informative error
    /// reports
    fn output2(&mut self) -> Result<TestOutput<NoDir>, Report>;

    /// wrapper for `spawn` fn on `Command` that constructs informative error
    /// reports
    fn spawn2<T>(&mut self, dir: T) -> Result<TestChild<T>, Report>;
}

impl CommandExt for Command {
    /// wrapper for `status` fn on `Command` that constructs informative error
    /// reports
    fn status2(&mut self) -> Result<TestStatus, Report> {
        let cmd = format!("{:?}", self);
        let status = self.status();

        let command = || cmd.clone().header("Command:");

        let status = status
            .wrap_err("failed to execute process")
            .with_section(command)?;

        Ok(TestStatus { cmd, status })
    }

    /// wrapper for `output` fn on `Command` that constructs informative error
    /// reports
    fn output2(&mut self) -> Result<TestOutput<NoDir>, Report> {
        let output = self.output();

        let output = output
            .wrap_err("failed to execute process")
            .with_section(|| format!("{:?}", self).header("Command:"))?;

        Ok(TestOutput {
            dir: None,
            output,
            cmd: format!("{:?}", self),
        })
    }

    /// wrapper for `spawn` fn on `Command` that constructs informative error
    /// reports
    fn spawn2<T>(&mut self, dir: T) -> Result<TestChild<T>, Report> {
        let cmd = format!("{:?}", self);
        let child = self.spawn();

        let child = child
            .wrap_err("failed to execute process")
            .with_section(|| cmd.clone().header("Command:"))?;

        Ok(TestChild {
            dir: Some(dir),
            cmd,
            child: Some(child),
            stdout: None,
            stderr: None,
            failure_regexes: RegexSet::empty(),
            ignore_regexes: RegexSet::empty(),
            deadline: None,
            bypass_test_capture: false,
        })
    }
}

/// Extension trait for methods on `tempdir::TempDir` for using it as a test
/// directory with an arbitrary command.
///
/// This trait is separate from `ZebradTestDirExt`, so that we can test
/// `zebra_test::command` without running `zebrad`.
pub trait TestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `cmd` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    fn spawn_child_with_command(self, cmd: &str, args: &[&str]) -> Result<TestChild<Self>>;
}

impl<T> TestDirExt for T
where
    Self: AsRef<Path> + Sized,
{
    fn spawn_child_with_command(self, cmd: &str, args: &[&str]) -> Result<TestChild<Self>> {
        let mut cmd = test_cmd(cmd, self.as_ref())?;

        Ok(cmd
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2(self)
            .unwrap())
    }
}

/// Test command exit status information.
#[derive(Debug)]
pub struct TestStatus {
    /// The original command string.
    pub cmd: String,

    /// The exit status of the command.
    pub status: ExitStatus,
}

impl TestStatus {
    pub fn assert_success(self) -> Result<Self> {
        if !self.status.success() {
            Err(eyre!("command exited unsuccessfully")).context_from(&self)?;
        }

        Ok(self)
    }

    pub fn assert_failure(self) -> Result<Self> {
        if self.status.success() {
            Err(eyre!("command unexpectedly exited successfully")).context_from(&self)?;
        }

        Ok(self)
    }
}

/// A test command child process.
// TODO: split this struct into its own module (or multiple modules)
#[derive(Debug)]
pub struct TestChild<T> {
    /// The working directory of the command.
    ///
    /// `None` when the command has been waited on,
    /// and its output has been taken.
    pub dir: Option<T>,

    /// The original command string.
    pub cmd: String,

    /// The child process itself.
    ///
    /// `None` when the command has been waited on,
    /// and its output has been taken.
    pub child: Option<Child>,

    /// The standard output stream of the child process.
    ///
    /// TODO: replace with `Option<ChildOutput { stdout, stderr }>.
    pub stdout: Option<Box<dyn IteratorDebug<Item = std::io::Result<String>>>>,

    /// The standard error stream of the child process.
    pub stderr: Option<Box<dyn IteratorDebug<Item = std::io::Result<String>>>>,

    /// Command outputs which indicate test failure.
    ///
    /// This list of regexes is matches against `stdout` or `stderr`,
    /// in every method that reads command output.
    ///
    /// If any line matches any failure regex, the test fails.
    failure_regexes: RegexSet,

    /// Command outputs which are ignored when checking for test failure.
    /// These regexes override `failure_regexes`.
    ///
    /// This list of regexes is matches against `stdout` or `stderr`,
    /// in every method that reads command output.
    ///
    /// If a line matches any ignore regex, the failure regex check is skipped for that line.
    ignore_regexes: RegexSet,

    /// The deadline for this command to finish.
    ///
    /// Only checked when the command outputs each new line (#1140).
    pub deadline: Option<Instant>,

    /// If true, write child output directly to standard output,
    /// bypassing the Rust test harness output capture.
    bypass_test_capture: bool,
}

/// Checks command output log `line` from `cmd` against a `failure_regexes` regex set,
/// and panics if any regex matches. The line is skipped if it matches `ignore_regexes`.
///
/// # Panics
///
/// - if any stdout or stderr lines match any failure regex, but do not match any ignore regex
pub fn check_failure_regexes(
    line: &std::io::Result<String>,
    failure_regexes: &RegexSet,
    ignore_regexes: &RegexSet,
    cmd: &str,
    bypass_test_capture: bool,
) {
    if let Ok(line) = line {
        let ignore_matches = ignore_regexes.matches(line);
        let ignore_matches: Vec<&str> = ignore_matches
            .iter()
            .map(|index| ignore_regexes.patterns()[index].as_str())
            .collect();

        let failure_matches = failure_regexes.matches(line);
        let failure_matches: Vec<&str> = failure_matches
            .iter()
            .map(|index| failure_regexes.patterns()[index].as_str())
            .collect();

        if !ignore_matches.is_empty() {
            let ignore_matches = ignore_matches.join(",");

            let ignore_msg = if failure_matches.is_empty() {
                format!(
                    "Log matched ignore regexes: {:?}, but no failure regexes",
                    ignore_matches,
                )
            } else {
                let failure_matches = failure_matches.join(",");
                format!(
                    "Ignoring failure regexes: {:?}, because log matched ignore regexes: {:?}",
                    failure_matches, ignore_matches,
                )
            };

            write_to_test_logs(ignore_msg, bypass_test_capture);
            return;
        }

        assert!(
            failure_matches.is_empty(),
            "test command:\n\
             {cmd}\n\n\
             Logged a failure message:\n\
             {line}\n\n\
             Matching failure regex: \
             {failure_matches:#?}\n\n\
             All Failure regexes: \
             {:#?}\n",
            failure_regexes.patterns(),
        );
    }
}

/// Write `line` to stdout, so it can be seen in the test logs.
///
/// Set `bypass_test_capture` to `true` or
/// use `cargo test -- --nocapture` to see this output.
///
/// May cause weird reordering for stdout / stderr.
/// Uses stdout even if the original lines were from stderr.
#[allow(clippy::print_stdout)]
fn write_to_test_logs<S>(line: S, bypass_test_capture: bool)
where
    S: AsRef<str>,
{
    let line = line.as_ref();

    if bypass_test_capture {
        // Send lines directly to the terminal (or process stdout file redirect).
        #[allow(clippy::explicit_write)]
        writeln!(std::io::stdout(), "{}", line).unwrap();
    } else {
        // If the test fails, the test runner captures and displays this output.
        // To show this output unconditionally, use `cargo test -- --nocapture`.
        println!("{}", line);
    }

    // Some OSes require a flush to send all output to the terminal.
    let _ = std::io::stdout().lock().flush();
}

/// A [`CollectRegexSet`] iterator that never matches anything.
///
/// Used to work around type inference issues in [`TestChild::with_failure_regex_iter`].
pub const NO_MATCHES_REGEX_ITER: &[&str] = &[];

impl<T> TestChild<T> {
    /// Sets up command output so each line is checked against a failure regex set,
    /// unless it matches any of the ignore regexes.
    ///
    /// The failure regexes are ignored by `wait_with_output`.
    ///
    /// To never match any log lines, use `RegexSet::empty()`.
    ///
    /// This method is a [`TestChild::with_failure_regexes`] wrapper for
    /// strings, [`Regex`]es, and [`RegexSet`]s.
    ///
    /// # Panics
    ///
    /// - adds a panic to any method that reads output,
    ///   if any stdout or stderr lines match any failure regex
    pub fn with_failure_regex_set<F, X>(self, failure_regexes: F, ignore_regexes: X) -> Self
    where
        F: ToRegexSet,
        X: ToRegexSet,
    {
        let failure_regexes = failure_regexes
            .to_regex_set()
            .expect("failure regexes must be valid");

        let ignore_regexes = ignore_regexes
            .to_regex_set()
            .expect("ignore regexes must be valid");

        self.with_failure_regexes(failure_regexes, ignore_regexes)
    }

    /// Sets up command output so each line is checked against a failure regex set,
    /// unless it matches any of the ignore regexes.
    ///
    /// The failure regexes are ignored by `wait_with_output`.
    ///
    /// To never match any log lines, use [`NO_MATCHES_REGEX_ITER`].
    ///
    /// This method is a [`TestChild::with_failure_regexes`] wrapper for
    /// regular expression iterators.
    ///
    /// # Panics
    ///
    /// - adds a panic to any method that reads output,
    ///   if any stdout or stderr lines match any failure regex
    pub fn with_failure_regex_iter<F, X>(self, failure_regexes: F, ignore_regexes: X) -> Self
    where
        F: CollectRegexSet,
        X: CollectRegexSet,
    {
        let failure_regexes = failure_regexes
            .collect_regex_set()
            .expect("failure regexes must be valid");

        let ignore_regexes = ignore_regexes
            .collect_regex_set()
            .expect("ignore regexes must be valid");

        self.with_failure_regexes(failure_regexes, ignore_regexes)
    }

    /// Sets up command output so each line is checked against a failure regex set,
    /// unless it matches any of the ignore regexes.
    ///
    /// The failure regexes are ignored by `wait_with_output`.
    ///
    /// # Panics
    ///
    /// - adds a panic to any method that reads output,
    ///   if any stdout or stderr lines match any failure regex
    pub fn with_failure_regexes(
        mut self,
        failure_regexes: RegexSet,
        ignore_regexes: impl Into<Option<RegexSet>>,
    ) -> Self {
        self.failure_regexes = failure_regexes;
        self.ignore_regexes = ignore_regexes.into().unwrap_or_else(RegexSet::empty);

        self.apply_failure_regexes_to_outputs();

        self
    }

    /// Applies the failure and ignore regex sets to command output.
    ///
    /// The failure regexes are ignored by `wait_with_output`.
    ///
    /// # Panics
    ///
    /// - adds a panic to any method that reads output,
    ///   if any stdout or stderr lines match any failure regex
    pub fn apply_failure_regexes_to_outputs(&mut self) {
        if self.stdout.is_none() {
            self.stdout = self
                .child
                .as_mut()
                .and_then(|child| child.stdout.take())
                .map(|output| self.map_into_string_lines(output))
        }

        if self.stderr.is_none() {
            self.stderr = self
                .child
                .as_mut()
                .and_then(|child| child.stderr.take())
                .map(|output| self.map_into_string_lines(output))
        }
    }

    /// Maps a reader into a string line iterator,
    /// and applies the failure and ignore regex sets to it.
    fn map_into_string_lines<R>(
        &self,
        reader: R,
    ) -> Box<dyn IteratorDebug<Item = std::io::Result<String>>>
    where
        R: Read + Debug + 'static,
    {
        let failure_regexes = self.failure_regexes.clone();
        let ignore_regexes = self.ignore_regexes.clone();
        let cmd = self.cmd.clone();
        let bypass_test_capture = self.bypass_test_capture;

        let reader = BufReader::new(reader);
        let lines = BufRead::lines(reader).inspect(move |line| {
            check_failure_regexes(
                line,
                &failure_regexes,
                &ignore_regexes,
                &cmd,
                bypass_test_capture,
            )
        });

        Box::new(lines) as _
    }

    /// Kill the child process.
    ///
    /// ## BUGS
    ///
    /// On Windows (and possibly macOS), this function can return `Ok` for
    /// processes that have panicked. See #1781.
    #[spandoc::spandoc]
    pub fn kill(&mut self) -> Result<()> {
        let child = match self.child.as_mut() {
            Some(child) => child,
            None => return Err(eyre!("child was already taken")).context_from(self.as_mut()),
        };

        /// SPANDOC: Killing child process
        child.kill().context_from(self.as_mut())?;

        Ok(())
    }

    /// Kill the process, and consume all its remaining output.
    ///
    /// Returns the result of the kill.
    pub fn kill_and_consume_output(&mut self) -> Result<()> {
        self.apply_failure_regexes_to_outputs();

        // Prevent a hang when consuming output,
        // by making sure the child's output actually finishes.
        let kill_result = self.kill();

        // Read unread child output.
        //
        // This checks for failure logs, and prevents some test hangs and deadlocks.
        if self.child.is_some() || self.stdout.is_some() {
            let wrote_lines = self.wait_for_stdout_line("Child Stdout:".to_string());

            while self.wait_for_stdout_line(None) {}

            if wrote_lines {
                // Write an empty line, to make output more readable
                self.write_to_test_logs("");
            }
        }

        if self.child.is_some() || self.stderr.is_some() {
            let wrote_lines = self.wait_for_stderr_line("Child Stderr:".to_string());

            while self.wait_for_stderr_line(None) {}

            if wrote_lines {
                self.write_to_test_logs("");
            }
        }

        kill_result
    }

    /// Waits until a line of standard output is available, then consumes it.
    ///
    /// If there is a line, and `write_context` is `Some`, writes the context to the test logs.
    /// Then writes the line to the test logs.
    ///
    /// Returns `true` if a line was available,
    /// or `false` if the standard output has finished.
    pub fn wait_for_stdout_line<OptS>(&mut self, write_context: OptS) -> bool
    where
        OptS: Into<Option<String>>,
    {
        self.apply_failure_regexes_to_outputs();

        if let Some(Ok(line)) = self.stdout.as_mut().and_then(|iter| iter.next()) {
            if let Some(write_context) = write_context.into() {
                self.write_to_test_logs(write_context);
            }

            self.write_to_test_logs(line);

            return true;
        }

        false
    }

    /// Waits until a line of standard error is available, then consumes it.
    ///
    /// If there is a line, and `write_context` is `Some`, writes the context to the test logs.
    /// Then writes the line to the test logs.
    ///
    /// Returns `true` if a line was available,
    /// or `false` if the standard error has finished.
    pub fn wait_for_stderr_line<OptS>(&mut self, write_context: OptS) -> bool
    where
        OptS: Into<Option<String>>,
    {
        self.apply_failure_regexes_to_outputs();

        if let Some(Ok(line)) = self.stderr.as_mut().and_then(|iter| iter.next()) {
            if let Some(write_context) = write_context.into() {
                self.write_to_test_logs(write_context);
            }

            self.write_to_test_logs(line);

            return true;
        }

        false
    }

    /// Waits for the child process to exit, then returns its output.
    ///
    /// The other test child output methods take one or both outputs,
    /// making them unavailable to this method.
    ///
    /// Ignores any configured timeouts.
    ///
    /// Returns an error if the child has already been taken,
    /// or both outputs have already been taken.
    #[spandoc::spandoc]
    pub fn wait_with_output(mut self) -> Result<TestOutput<T>> {
        let child = match self.child.take() {
            Some(child) => child,

            // Also checks the taken child output for failure regexes,
            // either in `context_from`, or on drop.
            None => {
                return Err(eyre!(
                    "child was already taken.\n\
                 wait_with_output can only be called once for each child process",
                ))
                .context_from(self.as_mut())
            }
        };

        // TODO: fix the usage in the zebrad acceptance tests, or fix the bugs in TestChild,
        //       then re-enable this check
        /*
        if child.stdout.is_none() && child.stderr.is_none() {
            // Also checks the taken child output for failure regexes,
            // either in `context_from`, or on drop.
            return Err(eyre!(
                "child stdout and stderr were already taken.\n\
                 Hint: choose one of these alternatives:\n\
                 1. use wait_with_output once on each child process, or\n\
                 2. replace wait_with_output with the other TestChild output methods"
            ))
            .context_from(self.as_mut());
        };
         */

        /// SPANDOC: waiting for command to exit
        let output = child.wait_with_output().with_section({
            let cmd = self.cmd.clone();
            || cmd.header("Command:")
        })?;

        Ok(TestOutput {
            output,
            cmd: self.cmd.clone(),
            dir: self.dir.take(),
        })
    }

    /// Set a timeout for `expect_stdout_line_matches` or `expect_stderr_line_matches`.
    ///
    /// Does not apply to `wait_with_output`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Some(Instant::now() + timeout);
        self
    }

    /// Configures testrunner to forward stdout and stderr to the true stdout,
    /// rather than the fakestdout used by cargo tests.
    pub fn bypass_test_capture(mut self, cond: bool) -> Self {
        self.bypass_test_capture = cond;
        self
    }

    /// Checks each line of the child's stdout against `success_regex`, and returns Ok
    /// if a line matches.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See `expect_line_matching` for details.
    #[instrument(skip(self))]
    pub fn expect_stdout_line_matches<R>(&mut self, success_regex: R) -> Result<&mut Self>
    where
        R: ToRegex + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stdout") {
            Ok(()) => {
                self.stdout = Some(lines);
                Ok(self)
            }
            Err(report) => Err(report),
        }
    }

    /// Checks each line of the child's stderr against `success_regex`, and returns Ok
    /// if a line matches.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See `expect_line_matching` for details.
    #[instrument(skip(self))]
    pub fn expect_stderr_line_matches<R>(&mut self, success_regex: R) -> Result<&mut Self>
    where
        R: ToRegex + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stderr
            .take()
            .expect("child must capture stderr to call expect_stderr_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stderr") {
            Ok(()) => {
                self.stderr = Some(lines);
                Ok(self)
            }
            Err(report) => Err(report),
        }
    }

    /// Checks each line in `lines` against a regex set, and returns Ok if a line matches.
    ///
    /// [`TestChild::expect_line_matching`] wrapper for strings, [`Regex`]es,
    /// and [`RegexSet`]s.
    pub fn expect_line_matching_regex_set<L, R>(
        &mut self,
        lines: &mut L,
        success_regexes: R,
        stream_name: &str,
    ) -> Result<()>
    where
        L: Iterator<Item = std::io::Result<String>>,
        R: ToRegexSet,
    {
        let success_regexes = success_regexes
            .to_regex_set()
            .expect("regexes must be valid");

        self.expect_line_matching_regexes(lines, success_regexes, stream_name)
    }

    /// Checks each line in `lines` against a regex set, and returns Ok if a line matches.
    ///
    /// [`TestChild::expect_line_matching`] wrapper for regular expression iterators.
    pub fn expect_line_matching_regex_iter<L, I>(
        &mut self,
        lines: &mut L,
        success_regexes: I,
        stream_name: &str,
    ) -> Result<()>
    where
        L: Iterator<Item = std::io::Result<String>>,
        I: CollectRegexSet,
    {
        let success_regexes = success_regexes
            .collect_regex_set()
            .expect("regexes must be valid");

        self.expect_line_matching_regexes(lines, success_regexes, stream_name)
    }

    /// Checks each line in `lines` against `success_regexes`, and returns Ok if a line
    /// matches. Uses `stream_name` as the name for `lines` in error reports.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    ///
    /// Note: the timeout is only checked after each full line is received from
    /// the child (#1140).
    #[instrument(skip(self, lines))]
    pub fn expect_line_matching_regexes<L>(
        &mut self,
        lines: &mut L,
        success_regexes: RegexSet,
        stream_name: &str,
    ) -> Result<()>
    where
        L: Iterator<Item = std::io::Result<String>>,
    {
        // We don't check `is_running` here,
        // because we want to read to the end of the buffered output,
        // even if the child process has exited.
        while !self.past_deadline() {
            let line = if let Some(line) = lines.next() {
                line?
            } else {
                // When the child process closes its output,
                // and we've read all of the buffered output,
                // stop checking for any more output.
                break;
            };

            // Since we're about to discard this line write it to stdout.
            self.write_to_test_logs(&line);

            if success_regexes.is_match(&line) {
                return Ok(());
            }
        }

        if self.is_running() {
            // If the process exits between is_running and kill, we will see
            // spurious errors here. If that happens, ignore "no such process"
            // errors from kill.
            self.kill()?;
        }

        let report = eyre!(
            "{} of command did not contain any matches for the given regex",
            stream_name
        )
        .context_from(self)
        .with_section(|| format!("{:#?}", success_regexes.patterns()).header("Match Regex:"));

        Err(report)
    }

    /// Write `line` to stdout, so it can be seen in the test logs.
    ///
    /// Use [bypass_test_capture(true)](TestChild::bypass_test_capture) or
    /// `cargo test -- --nocapture` to see this output.
    ///
    /// May cause weird reordering for stdout / stderr.
    /// Uses stdout even if the original lines were from stderr.
    #[allow(clippy::print_stdout)]
    fn write_to_test_logs<S>(&self, line: S)
    where
        S: AsRef<str>,
    {
        write_to_test_logs(line, self.bypass_test_capture);
    }

    /// Kill `child`, wait for its output, and use that output as the context for
    /// an error report based on `error`.
    #[instrument(skip(self, result))]
    pub fn kill_on_error<V, E>(mut self, result: Result<V, E>) -> Result<(V, Self), Report>
    where
        E: Into<Report> + Send + Sync + 'static,
    {
        let mut error: Report = match result {
            Ok(success) => return Ok((success, self)),
            Err(error) => error.into(),
        };

        if self.is_running() {
            let kill_res = self.kill();
            if let Err(kill_err) = kill_res {
                error = error.wrap_err(kill_err);
            }
        }

        let output_res = self.wait_with_output();
        let error = match output_res {
            Err(output_err) => error.wrap_err(output_err),
            Ok(output) => error.context_from(&output),
        };

        Err(error)
    }

    fn past_deadline(&self) -> bool {
        self.deadline
            .map(|deadline| Instant::now() > deadline)
            .unwrap_or(false)
    }

    /// Is this process currently running?
    ///
    /// ## Bugs
    ///
    /// On Windows and macOS, this function can return `true` for processes that
    /// have panicked. See #1781.
    ///
    /// ## Panics
    ///
    /// If the child process was already been taken using wait_with_output.
    pub fn is_running(&mut self) -> bool {
        matches!(
            self.child
                .as_mut()
                .expect("child has not been taken")
                .try_wait(),
            Ok(None),
        )
    }
}

impl<T> AsRef<TestChild<T>> for TestChild<T> {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<T> AsMut<TestChild<T>> for TestChild<T> {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

impl<T> Drop for TestChild<T> {
    fn drop(&mut self) {
        // Clean up child processes when the test finishes,
        // and check for failure logs.
        //
        // We don't care about the kill result here.
        let _ = self.kill_and_consume_output();
    }
}

/// Test command output logs.
// TODO: split this struct into its own module
#[derive(Debug)]
pub struct TestOutput<T> {
    /// The test directory for this test output.
    ///
    /// Keeps the test dir around from `TestChild`,
    /// so it doesn't get deleted during `wait_with_output`.
    #[allow(dead_code)]
    pub dir: Option<T>,

    /// The test command for this test output.
    pub cmd: String,

    /// The test exit status, standard out, and standard error.
    pub output: Output,
}

impl<T> TestOutput<T> {
    pub fn assert_success(self) -> Result<Self> {
        if !self.output.status.success() {
            Err(eyre!("command exited unsuccessfully")).context_from(&self)?;
        }

        Ok(self)
    }

    pub fn assert_failure(self) -> Result<Self> {
        if self.output.status.success() {
            Err(eyre!("command unexpectedly exited successfully")).context_from(&self)?;
        }

        Ok(self)
    }

    /// Checks the output of a command, using a closure to determine if the
    /// output is valid.
    ///
    /// If the closure returns `true`, the check returns `Ok(self)`.
    /// If the closure returns `false`, the check returns an error containing
    /// `output_name` and `err_msg`, with context from the command.
    ///
    /// `output` is typically `self.output.stdout` or `self.output.stderr`.
    #[instrument(skip(self, output_predicate, output))]
    pub fn output_check<P>(
        &self,
        output_predicate: P,
        output: &[u8],
        output_name: impl ToString + fmt::Debug,
        err_msg: impl ToString + fmt::Debug,
    ) -> Result<&Self>
    where
        P: FnOnce(&str) -> bool,
    {
        let output = String::from_utf8_lossy(output);

        if output_predicate(&output) {
            Ok(self)
        } else {
            Err(eyre!(
                "{} of command did not {}",
                output_name.to_string(),
                err_msg.to_string()
            ))
            .context_from(self)
        }
    }

    /// Checks each line in the output of a command, using a closure to determine
    /// if the line is valid.
    ///
    /// See [`output_check`] for details.
    #[instrument(skip(self, line_predicate, output))]
    pub fn any_output_line<P>(
        &self,
        mut line_predicate: P,
        output: &[u8],
        output_name: impl ToString + fmt::Debug,
        err_msg: impl ToString + fmt::Debug,
    ) -> Result<&Self>
    where
        P: FnMut(&str) -> bool,
    {
        let output_predicate = |stdout: &str| {
            for line in stdout.lines() {
                if line_predicate(line) {
                    return true;
                }
            }
            false
        };

        self.output_check(
            output_predicate,
            output,
            output_name,
            format!("have any lines that {}", err_msg.to_string()),
        )
    }

    /// Tests if any lines in the output of a command contain `s`.
    ///
    /// See [`any_output_line`] for details.
    #[instrument(skip(self, output))]
    pub fn any_output_line_contains(
        &self,
        s: &str,
        output: &[u8],
        output_name: impl ToString + fmt::Debug,
        err_msg: impl ToString + fmt::Debug,
    ) -> Result<&Self> {
        self.any_output_line(
            |line| line.contains(s),
            output,
            output_name,
            format!("contain {}", err_msg.to_string()),
        )
        .with_section(|| format!("{:?}", s).header("Match String:"))
    }

    /// Tests if standard output contains `s`.
    #[instrument(skip(self))]
    pub fn stdout_contains(&self, s: &str) -> Result<&Self> {
        self.output_check(
            |stdout| stdout.contains(s),
            &self.output.stdout,
            "stdout",
            "contain the given string",
        )
        .with_section(|| format!("{:?}", s).header("Match String:"))
    }

    /// Tests if standard output matches `regex`.
    #[instrument(skip(self))]
    pub fn stdout_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegex + Debug,
    {
        let re = regex.to_regex().expect("regex must be valid");

        self.output_check(
            |stdout| re.is_match(stdout),
            &self.output.stdout,
            "stdout",
            "matched the given regex",
        )
        .with_section(|| format!("{:?}", regex).header("Match Regex:"))
    }

    /// Tests if any lines in standard output contain `s`.
    #[instrument(skip(self))]
    pub fn stdout_line_contains(&self, s: &str) -> Result<&Self> {
        self.any_output_line_contains(s, &self.output.stdout, "stdout", "the given string")
    }

    /// Tests if any lines in standard output match `regex`.
    #[instrument(skip(self))]
    pub fn stdout_line_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegex + Debug,
    {
        let re = regex.to_regex().expect("regex must be valid");

        self.any_output_line(
            |line| re.is_match(line),
            &self.output.stdout,
            "stdout",
            "matched the given regex",
        )
        .with_section(|| format!("{:?}", regex).header("Line Match Regex:"))
    }

    /// Tests if standard error contains `s`.
    #[instrument(skip(self))]
    pub fn stderr_contains(&self, s: &str) -> Result<&Self> {
        self.output_check(
            |stderr| stderr.contains(s),
            &self.output.stderr,
            "stderr",
            "contain the given string",
        )
        .with_section(|| format!("{:?}", s).header("Match String:"))
    }

    /// Tests if standard error matches `regex`.
    #[instrument(skip(self))]
    pub fn stderr_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegex + Debug,
    {
        let re = regex.to_regex().expect("regex must be valid");

        self.output_check(
            |stderr| re.is_match(stderr),
            &self.output.stderr,
            "stderr",
            "matched the given regex",
        )
        .with_section(|| format!("{:?}", regex).header("Match Regex:"))
    }

    /// Tests if any lines in standard error contain `s`.
    #[instrument(skip(self))]
    pub fn stderr_line_contains(&self, s: &str) -> Result<&Self> {
        self.any_output_line_contains(s, &self.output.stderr, "stderr", "the given string")
    }

    /// Tests if any lines in standard error match `regex`.
    #[instrument(skip(self))]
    pub fn stderr_line_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegex + Debug,
    {
        let re = regex.to_regex().expect("regex must be valid");

        self.any_output_line(
            |line| re.is_match(line),
            &self.output.stderr,
            "stderr",
            "matched the given regex",
        )
        .with_section(|| format!("{:?}", regex).header("Line Match Regex:"))
    }

    /// Returns Ok if the program was killed, Err(Report) if exit was by another
    /// reason.
    pub fn assert_was_killed(&self) -> Result<()> {
        if self.was_killed() {
            Ok(())
        } else {
            Err(eyre!(
                "command exited without a kill, but the test expected kill exit"
            ))
            .context_from(self)?
        }
    }

    /// Returns Ok if the program was not killed, Err(Report) if exit was by
    /// another reason.
    pub fn assert_was_not_killed(&self) -> Result<()> {
        if self.was_killed() {
            Err(eyre!(
                "command was killed, but the test expected an exit without a kill"
            ))
            .context_from(self)?
        } else {
            Ok(())
        }
    }

    #[cfg(not(unix))]
    fn was_killed(&self) -> bool {
        self.output.status.code() == Some(1)
    }

    #[cfg(unix)]
    fn was_killed(&self) -> bool {
        self.output.status.signal() == Some(9)
    }
}

/// Add context to an error report
// TODO: split this trait into its own module
pub trait ContextFrom<S> {
    type Return;

    fn context_from(self, source: S) -> Self::Return;
}

impl<C, T, E> ContextFrom<C> for Result<T, E>
where
    E: Into<Report>,
    Report: ContextFrom<C, Return = Report>,
{
    type Return = Result<T, Report>;

    fn context_from(self, source: C) -> Self::Return {
        self.map_err(|e| e.into())
            .map_err(|report| report.context_from(source))
    }
}

impl ContextFrom<&TestStatus> for Report {
    type Return = Report;

    fn context_from(self, source: &TestStatus) -> Self::Return {
        let command = || source.cmd.clone().header("Command:");

        self.with_section(command).context_from(&source.status)
    }
}

impl<T> ContextFrom<&mut TestChild<T>> for Report {
    type Return = Report;

    #[allow(clippy::print_stdout)]
    fn context_from(mut self, source: &mut TestChild<T>) -> Self::Return {
        self = self.section(source.cmd.clone().header("Command:"));

        if let Some(child) = &mut source.child {
            if let Ok(Some(status)) = child.try_wait() {
                self = self.context_from(&status);
            }
        }

        // Reading test child process output could hang if the child process is still running,
        // so kill it first.
        if let Some(child) = source.child.as_mut() {
            let _ = child.kill();
        }

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        if let Some(stdout) = &mut source.stdout {
            for line in stdout {
                let line = if let Ok(line) = line { line } else { break };
                let _ = writeln!(&mut stdout_buf, "{}", line);
            }
        } else if let Some(child) = &mut source.child {
            if let Some(stdout) = &mut child.stdout {
                let _ = stdout.read_to_string(&mut stdout_buf);
            }
        }

        if let Some(stderr) = &mut source.stderr {
            for line in stderr {
                let line = if let Ok(line) = line { line } else { break };
                let _ = writeln!(&mut stderr_buf, "{}", line);
            }
        } else if let Some(child) = &mut source.child {
            if let Some(stderr) = &mut child.stderr {
                let _ = stderr.read_to_string(&mut stderr_buf);
            }
        }

        self.section(stdout_buf.header("Unread Stdout:"))
            .section(stderr_buf.header("Unread Stderr:"))
    }
}

impl<T> ContextFrom<&TestOutput<T>> for Report {
    type Return = Report;

    fn context_from(self, source: &TestOutput<T>) -> Self::Return {
        self.with_section(|| source.cmd.clone().header("Command:"))
            .context_from(&source.output)
    }
}

impl ContextFrom<&Output> for Report {
    type Return = Report;

    fn context_from(self, source: &Output) -> Self::Return {
        let stdout = || {
            String::from_utf8_lossy(&source.stdout)
                .into_owned()
                .header("Stdout:")
        };
        let stderr = || {
            String::from_utf8_lossy(&source.stderr)
                .into_owned()
                .header("Stderr:")
        };

        self.context_from(&source.status)
            .with_section(stdout)
            .with_section(stderr)
    }
}

impl ContextFrom<&ExitStatus> for Report {
    type Return = Report;

    fn context_from(self, source: &ExitStatus) -> Self::Return {
        let how = if source.success() {
            "successfully"
        } else {
            "unsuccessfully"
        };

        if let Some(code) = source.code() {
            return self.with_section(|| {
                format!("command exited {} with status code {}", how, code).header("Exit Status:")
            });
        }

        #[cfg(unix)]
        if let Some(signal) = source.signal() {
            self.with_section(|| {
                format!("command terminated {} by signal {}", how, signal).header("Exit Status:")
            })
        } else {
            unreachable!("on unix all processes either terminate via signal or with an exit code");
        }

        #[cfg(not(unix))]
        self.with_section(|| {
            format!("command exited {} without a status code or signal", how).header("Exit Status:")
        })
    }
}
