//! Launching test commands for Zebra integration and acceptance tests.

use std::{
    collections::HashSet,
    convert::Infallible as NoDir,
    fmt::{self, Debug, Write as _},
    io::{BufRead, BufReader, ErrorKind, Read, Write as _},
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

#[macro_use]
mod arguments;

pub mod to_regex;

pub use self::arguments::Arguments;
use self::to_regex::{CollectRegexSet, RegexSetExt, ToRegexSet};

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

/// Wrappers for `Command` methods to integrate with [`zebra_test`](crate).
pub trait CommandExt {
    /// wrapper for `status` fn on `Command` that constructs informative error
    /// reports
    fn status2(&mut self) -> Result<TestStatus, Report>;

    /// wrapper for `output` fn on `Command` that constructs informative error
    /// reports
    fn output2(&mut self) -> Result<TestOutput<NoDir>, Report>;

    /// wrapper for `spawn` fn on `Command` that constructs informative error
    /// reports using the original `command_path`
    fn spawn2<T>(&mut self, dir: T, command_path: impl ToString) -> Result<TestChild<T>, Report>;
}

impl CommandExt for Command {
    /// wrapper for `status` fn on `Command` that constructs informative error
    /// reports
    fn status2(&mut self) -> Result<TestStatus, Report> {
        let cmd = format!("{self:?}");
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
            .with_section(|| format!("{self:?}").header("Command:"))?;

        Ok(TestOutput {
            dir: None,
            output,
            cmd: format!("{self:?}"),
        })
    }

    /// wrapper for `spawn` fn on `Command` that constructs informative error
    /// reports using the original `command_path`
    fn spawn2<T>(&mut self, dir: T, command_path: impl ToString) -> Result<TestChild<T>, Report> {
        let command_and_args = format!("{self:?}");
        let child = self.spawn();

        let child = child
            .wrap_err("failed to execute process")
            .with_section(|| command_and_args.clone().header("Command:"))?;

        Ok(TestChild {
            dir: Some(dir),
            cmd: command_and_args,
            command_path: command_path.to_string(),
            child: Some(child),
            stdout: None,
            stderr: None,
            failure_regexes: RegexSet::empty(),
            ignore_regexes: RegexSet::empty(),
            deadline: None,
            timeout: None,
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
    fn spawn_child_with_command(self, cmd: &str, args: Arguments) -> Result<TestChild<Self>>;
}

impl<T> TestDirExt for T
where
    Self: AsRef<Path> + Sized,
{
    #[allow(clippy::unwrap_in_result)]
    fn spawn_child_with_command(
        self,
        command_path: &str,
        args: Arguments,
    ) -> Result<TestChild<Self>> {
        let mut cmd = test_cmd(command_path, self.as_ref())?;

        Ok(cmd
            .args(args.into_arguments())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2(self, command_path)
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

    /// The full command string, including arguments and working directory.
    pub cmd: String,

    /// The path of the command, as passed to spawn2().
    pub command_path: String,

    /// The child process itself.
    ///
    /// `None` when the command has been waited on,
    /// and its output has been taken.
    pub child: Option<Child>,

    /// The standard output stream of the child process.
    ///
    /// TODO: replace with `Option<ChildOutput { stdout, stderr }>.
    pub stdout: Option<Box<dyn IteratorDebug<Item = std::io::Result<String>> + Send>>,

    /// The standard error stream of the child process.
    pub stderr: Option<Box<dyn IteratorDebug<Item = std::io::Result<String>> + Send>>,

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

    /// The timeout for this command to finish.
    ///
    /// Only used for debugging output.
    pub timeout: Option<Duration>,

    /// If true, write child output directly to standard output,
    /// bypassing the Rust test harness output capture.
    bypass_test_capture: bool,
}

/// Checks command output log `line` from `cmd` against a `failure_regexes` regex set,
/// and returns an error if any regex matches. The line is skipped if it matches `ignore_regexes`.
///
/// Passes through errors from the underlying reader.
pub fn check_failure_regexes(
    line: std::io::Result<String>,
    failure_regexes: &RegexSet,
    ignore_regexes: &RegexSet,
    cmd: &str,
    bypass_test_capture: bool,
) -> std::io::Result<String> {
    let line = line?;

    // Check if the line matches any patterns
    let ignore_matches = ignore_regexes.matches(&line);
    let ignore_matches: Vec<&str> = ignore_matches
        .iter()
        .map(|index| ignore_regexes.patterns()[index].as_str())
        .collect();

    let failure_matches = failure_regexes.matches(&line);
    let failure_matches: Vec<&str> = failure_matches
        .iter()
        .map(|index| failure_regexes.patterns()[index].as_str())
        .collect();

    // If we match an ignore pattern, ignore any failure matches
    if !ignore_matches.is_empty() {
        let ignore_matches = ignore_matches.join(",");

        let ignore_msg = if failure_matches.is_empty() {
            format!("Log matched ignore regexes: {ignore_matches:?}, but no failure regexes",)
        } else {
            let failure_matches = failure_matches.join(",");
            format!(
                "Ignoring failure regexes: {failure_matches:?}, because log matched ignore regexes: {ignore_matches:?}",
            )
        };

        write_to_test_logs(ignore_msg, bypass_test_capture);

        return Ok(line);
    }

    // If there were no failures, pass the log line through
    if failure_matches.is_empty() {
        return Ok(line);
    }

    // Otherwise, if the process logged a failure message, return an error
    let error = std::io::Error::other(format!(
        "test command:\n\
             {cmd}\n\n\
             Logged a failure message:\n\
             {line}\n\n\
             Matching failure regex: \
             {failure_matches:#?}\n\n\
             All Failure regexes: \
             {:#?}\n",
        failure_regexes.patterns(),
    ));

    Err(error)
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
        writeln!(std::io::stdout(), "{line}").unwrap();
    } else {
        // If the test fails, the test runner captures and displays this output.
        // To show this output unconditionally, use `cargo test -- --nocapture`.
        println!("{line}");
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
    /// strings, `Regex`es, and [`RegexSet`]s.
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
    ) -> Box<dyn IteratorDebug<Item = std::io::Result<String>> + Send>
    where
        R: Read + Debug + Send + 'static,
    {
        let failure_regexes = self.failure_regexes.clone();
        let ignore_regexes = self.ignore_regexes.clone();
        let cmd = self.cmd.clone();
        let bypass_test_capture = self.bypass_test_capture;

        let reader = BufReader::new(reader);
        let lines = BufRead::lines(reader).map(move |line| {
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
    /// If `ignore_exited` is `true`, log "can't kill an exited process" errors,
    /// rather than returning them.
    ///
    /// Returns the result of the kill.
    ///
    /// ## BUGS
    ///
    /// On Windows (and possibly macOS), this function can return `Ok` for
    /// processes that have panicked. See #1781.
    #[spandoc::spandoc]
    pub fn kill(&mut self, ignore_exited: bool) -> Result<()> {
        let child = match self.child.as_mut() {
            Some(child) => child,
            None if ignore_exited => {
                Self::write_to_test_logs(
                    "test child was already taken\n\
                     ignoring kill because ignore_exited is true",
                    self.bypass_test_capture,
                );
                return Ok(());
            }
            None => {
                return Err(eyre!(
                    "test child was already taken\n\
                     call kill() once for each child process, or set ignore_exited to true"
                ))
                .context_from(self.as_mut())
            }
        };

        /// SPANDOC: Killing child process
        let kill_result = child.kill().or_else(|error| {
            if ignore_exited && error.kind() == ErrorKind::InvalidInput {
                Ok(())
            } else {
                Err(error)
            }
        });

        kill_result.context_from(self.as_mut())?;

        Ok(())
    }

    /// Kill the process, and consume all its remaining output.
    ///
    /// If `ignore_exited` is `true`, log "can't kill an exited process" errors,
    /// rather than returning them.
    ///
    /// Returns the result of the kill.
    pub fn kill_and_consume_output(&mut self, ignore_exited: bool) -> Result<()> {
        self.apply_failure_regexes_to_outputs();

        // Prevent a hang when consuming output,
        // by making sure the child's output actually finishes.
        let kill_result = self.kill(ignore_exited);

        // Read unread child output.
        //
        // This checks for failure logs, and prevents some test hangs and deadlocks.
        //
        // TODO: this could block if stderr is full and stdout is waiting for stderr to be read.
        if self.stdout.is_some() {
            let wrote_lines =
                self.wait_for_stdout_line(format!("\n{} Child Stdout:", self.command_path));

            while self.wait_for_stdout_line(None) {}

            if wrote_lines {
                // Write an empty line, to make output more readable
                Self::write_to_test_logs("", self.bypass_test_capture);
            }
        }

        if self.stderr.is_some() {
            let wrote_lines =
                self.wait_for_stderr_line(format!("\n{} Child Stderr:", self.command_path));

            while self.wait_for_stderr_line(None) {}

            if wrote_lines {
                Self::write_to_test_logs("", self.bypass_test_capture);
            }
        }

        kill_result
    }

    /// Kill the process, and return all its remaining standard output and standard error output.
    ///
    /// If `ignore_exited` is `true`, log "can't kill an exited process" errors,
    /// rather than returning them.
    ///
    /// Returns `Ok(output)`, or an error if the kill failed.
    pub fn kill_and_return_output(&mut self, ignore_exited: bool) -> Result<String> {
        self.apply_failure_regexes_to_outputs();

        // Prevent a hang when consuming output,
        // by making sure the child's output actually finishes.
        let kill_result = self.kill(ignore_exited);

        // Read unread child output.
        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        // This also checks for failure logs, and prevents some test hangs and deadlocks.
        loop {
            let mut remaining_output = false;

            if let Some(stdout) = self.stdout.as_mut() {
                if let Some(line) =
                    Self::wait_and_return_output_line(stdout, self.bypass_test_capture)
                {
                    stdout_buf.push_str(&line);
                    remaining_output = true;
                }
            }

            if let Some(stderr) = self.stderr.as_mut() {
                if let Some(line) =
                    Self::wait_and_return_output_line(stderr, self.bypass_test_capture)
                {
                    stderr_buf.push_str(&line);
                    remaining_output = true;
                }
            }

            if !remaining_output {
                break;
            }
        }

        let mut output = stdout_buf;
        output.push_str(&stderr_buf);

        kill_result.map(|()| output)
    }

    /// Waits until a line of standard output is available, then consumes it.
    ///
    /// If there is a line, and `write_context` is `Some`, writes the context to the test logs.
    /// Always writes the line to the test logs.
    ///
    /// Returns `true` if a line was available,
    /// or `false` if the standard output has finished.
    pub fn wait_for_stdout_line<OptS>(&mut self, write_context: OptS) -> bool
    where
        OptS: Into<Option<String>>,
    {
        self.apply_failure_regexes_to_outputs();

        if let Some(line_result) = self.stdout.as_mut().and_then(|iter| iter.next()) {
            let bypass_test_capture = self.bypass_test_capture;

            if let Some(write_context) = write_context.into() {
                Self::write_to_test_logs(write_context, bypass_test_capture);
            }

            Self::write_to_test_logs(
                line_result
                    .context_from(self)
                    .expect("failure reading test process logs"),
                bypass_test_capture,
            );

            return true;
        }

        false
    }

    /// Waits until a line of standard error is available, then consumes it.
    ///
    /// If there is a line, and `write_context` is `Some`, writes the context to the test logs.
    /// Always writes the line to the test logs.
    ///
    /// Returns `true` if a line was available,
    /// or `false` if the standard error has finished.
    pub fn wait_for_stderr_line<OptS>(&mut self, write_context: OptS) -> bool
    where
        OptS: Into<Option<String>>,
    {
        self.apply_failure_regexes_to_outputs();

        if let Some(line_result) = self.stderr.as_mut().and_then(|iter| iter.next()) {
            let bypass_test_capture = self.bypass_test_capture;

            if let Some(write_context) = write_context.into() {
                Self::write_to_test_logs(write_context, bypass_test_capture);
            }

            Self::write_to_test_logs(
                line_result
                    .context_from(self)
                    .expect("failure reading test process logs"),
                bypass_test_capture,
            );

            return true;
        }

        false
    }

    /// Waits until a line of `output` is available, then returns it.
    ///
    /// If there is a line, and `write_context` is `Some`, writes the context to the test logs.
    /// Always writes the line to the test logs.
    ///
    /// Returns `true` if a line was available,
    /// or `false` if the standard output has finished.
    #[allow(clippy::unwrap_in_result)]
    fn wait_and_return_output_line(
        mut output: impl Iterator<Item = std::io::Result<String>>,
        bypass_test_capture: bool,
    ) -> Option<String> {
        if let Some(line_result) = output.next() {
            let line_result = line_result.expect("failure reading test process logs");

            Self::write_to_test_logs(&line_result, bypass_test_capture);

            return Some(line_result);
        }

        None
    }

    /// Waits for the child process to exit, then returns its output.
    ///
    /// # Correctness
    ///
    /// The other test child output methods take one or both outputs,
    /// making them unavailable to this method.
    ///
    /// Ignores any configured timeouts.
    ///
    /// Returns an error if the child has already been taken.
    /// TODO: return an error if both outputs have already been taken.
    #[spandoc::spandoc]
    pub fn wait_with_output(mut self) -> Result<TestOutput<T>> {
        let child = match self.child.take() {
            Some(child) => child,

            // Also checks the taken child output for failure regexes,
            // either in `context_from`, or on drop.
            None => {
                return Err(eyre!(
                    "test child was already taken\n\
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
        self.timeout = Some(timeout);
        self.deadline = Some(Instant::now() + timeout);

        self
    }

    /// Configures this test runner to forward stdout and stderr to the true stdout,
    /// rather than the fakestdout used by cargo tests.
    pub fn bypass_test_capture(mut self, cond: bool) -> Self {
        self.bypass_test_capture = cond;
        self
    }

    /// Checks each line of the child's stdout against any regex in `success_regex`,
    /// and returns the first matching line. Prints all stdout lines.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    //
    // TODO: these methods could block if stderr is full and stdout is waiting for stderr to be read
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stdout_line_matches<R>(&mut self, success_regex: R) -> Result<String>
    where
        R: ToRegexSet + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stdout", true) {
            Ok(line) => {
                // Replace the log lines for the next check
                self.stdout = Some(lines);
                Ok(line)
            }
            Err(report) => {
                // Read all the log lines for error context
                self.stdout = Some(lines);
                Err(report).context_from(self)
            }
        }
    }

    /// Checks each line of the child's stderr against any regex in `success_regex`,
    /// and returns the first matching line. Prints all stderr lines to stdout.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stderr_line_matches<R>(&mut self, success_regex: R) -> Result<String>
    where
        R: ToRegexSet + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stderr
            .take()
            .expect("child must capture stderr to call expect_stderr_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stderr", true) {
            Ok(line) => {
                // Replace the log lines for the next check
                self.stderr = Some(lines);
                Ok(line)
            }
            Err(report) => {
                // Read all the log lines for error context
                self.stderr = Some(lines);
                Err(report).context_from(self)
            }
        }
    }

    /// Checks each line of the child's stdout, until it finds every regex in `unordered_regexes`,
    /// and returns all lines matched by any regex, until each regex has been matched at least once.
    /// If the output finishes or the command times out before all regexes are matched, returns an error with
    /// a list of unmatched regexes. Prints all stdout lines.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    //
    // TODO: these methods could block if stderr is full and stdout is waiting for stderr to be read
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stdout_line_matches_all_unordered<RegexList>(
        &mut self,
        unordered_regexes: RegexList,
    ) -> Result<Vec<String>>
    where
        RegexList: IntoIterator + Debug,
        RegexList::Item: ToRegexSet,
    {
        let regex_list = unordered_regexes.collect_regex_set()?;

        let mut unmatched_indexes: HashSet<usize> = (0..regex_list.len()).collect();
        let mut matched_lines = Vec::new();

        while !unmatched_indexes.is_empty() {
            let line = self
                .expect_stdout_line_matches(regex_list.clone())
                .map_err(|err| {
                    let unmatched_regexes = regex_list.patterns_for_indexes(&unmatched_indexes);

                    err.with_section(|| {
                        format!("{unmatched_regexes:#?}").header("Unmatched regexes:")
                    })
                    .with_section(|| format!("{matched_lines:#?}").header("Matched lines:"))
                })?;

            let matched_indices: HashSet<usize> = regex_list.matches(&line).iter().collect();
            unmatched_indexes = &unmatched_indexes - &matched_indices;

            matched_lines.push(line);
        }

        Ok(matched_lines)
    }

    /// Checks each line of the child's stderr, until it finds every regex in `unordered_regexes`,
    /// and returns all lines matched by any regex, until each regex has been matched at least once.
    /// If the output finishes or the command times out before all regexes are matched, returns an error with
    /// a list of unmatched regexes. Prints all stderr lines.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    //
    // TODO: these methods could block if stdout is full and stderr is waiting for stdout to be read
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stderr_line_matches_all_unordered<RegexList>(
        &mut self,
        unordered_regexes: RegexList,
    ) -> Result<Vec<String>>
    where
        RegexList: IntoIterator + Debug,
        RegexList::Item: ToRegexSet,
    {
        let regex_list = unordered_regexes.collect_regex_set()?;

        let mut unmatched_indexes: HashSet<usize> = (0..regex_list.len()).collect();
        let mut matched_lines = Vec::new();

        while !unmatched_indexes.is_empty() {
            let line = self
                .expect_stderr_line_matches(regex_list.clone())
                .map_err(|err| {
                    let unmatched_regexes = regex_list.patterns_for_indexes(&unmatched_indexes);

                    err.with_section(|| {
                        format!("{unmatched_regexes:#?}").header("Unmatched regexes:")
                    })
                    .with_section(|| format!("{matched_lines:#?}").header("Matched lines:"))
                })?;

            let matched_indices: HashSet<usize> = regex_list.matches(&line).iter().collect();
            unmatched_indexes = &unmatched_indexes - &matched_indices;

            matched_lines.push(line);
        }

        Ok(matched_lines)
    }

    /// Checks each line of the child's stdout against `success_regex`,
    /// and returns the first matching line. Does not print any output.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stdout_line_matches_silent<R>(&mut self, success_regex: R) -> Result<String>
    where
        R: ToRegexSet + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stdout", false) {
            Ok(line) => {
                // Replace the log lines for the next check
                self.stdout = Some(lines);
                Ok(line)
            }
            Err(report) => {
                // Read all the log lines for error context
                self.stdout = Some(lines);
                Err(report).context_from(self)
            }
        }
    }

    /// Checks each line of the child's stderr against `success_regex`,
    /// and returns the first matching line. Does not print any output.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    /// See [`Self::expect_line_matching_regex_set`] for details.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_stderr_line_matches_silent<R>(&mut self, success_regex: R) -> Result<String>
    where
        R: ToRegexSet + Debug,
    {
        self.apply_failure_regexes_to_outputs();

        let mut lines = self
            .stderr
            .take()
            .expect("child must capture stderr to call expect_stderr_line_matches, and it can't be called again after an error");

        match self.expect_line_matching_regex_set(&mut lines, success_regex, "stderr", false) {
            Ok(line) => {
                // Replace the log lines for the next check
                self.stderr = Some(lines);
                Ok(line)
            }
            Err(report) => {
                // Read all the log lines for error context
                self.stderr = Some(lines);
                Err(report).context_from(self)
            }
        }
    }

    /// Checks each line in `lines` against a regex set, and returns Ok if a line matches.
    ///
    /// [`Self::expect_line_matching_regexes`] wrapper for strings,
    /// [`Regex`](regex::Regex)es, and [`RegexSet`]s.
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_line_matching_regex_set<L, R>(
        &mut self,
        lines: &mut L,
        success_regexes: R,
        stream_name: &str,
        write_to_logs: bool,
    ) -> Result<String>
    where
        L: Iterator<Item = std::io::Result<String>>,
        R: ToRegexSet,
    {
        let success_regexes = success_regexes
            .to_regex_set()
            .expect("regexes must be valid");

        self.expect_line_matching_regexes(lines, success_regexes, stream_name, write_to_logs)
    }

    /// Checks each line in `lines` against a regex set, and returns Ok if a line matches.
    ///
    /// [`Self::expect_line_matching_regexes`] wrapper for regular expression iterators.
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_line_matching_regex_iter<L, I>(
        &mut self,
        lines: &mut L,
        success_regexes: I,
        stream_name: &str,
        write_to_logs: bool,
    ) -> Result<String>
    where
        L: Iterator<Item = std::io::Result<String>>,
        I: CollectRegexSet,
    {
        let success_regexes = success_regexes
            .collect_regex_set()
            .expect("regexes must be valid");

        self.expect_line_matching_regexes(lines, success_regexes, stream_name, write_to_logs)
    }

    /// Checks each line in `lines` against `success_regexes`, and returns Ok if a line
    /// matches. Uses `stream_name` as the name for `lines` in error reports.
    ///
    /// Kills the child on error, or after the configured timeout has elapsed.
    ///
    /// Note: the timeout is only checked after each full line is received from
    /// the child (#1140).
    #[instrument(skip(self, lines))]
    #[allow(clippy::unwrap_in_result)]
    pub fn expect_line_matching_regexes<L>(
        &mut self,
        lines: &mut L,
        success_regexes: RegexSet,
        stream_name: &str,
        write_to_logs: bool,
    ) -> Result<String>
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

            if write_to_logs {
                // Since we're about to discard this line write it to stdout.
                Self::write_to_test_logs(&line, self.bypass_test_capture);
            }

            if success_regexes.is_match(&line) {
                return Ok(line);
            }
        }

        if self.is_running() {
            // If the process exits between is_running and kill, we will see
            // spurious errors here. So we want to ignore "no such process"
            // errors from kill.
            self.kill(true)?;
        }

        let timeout = self
            .timeout
            .map(|timeout| humantime::format_duration(timeout).to_string())
            .unwrap_or_else(|| "unlimited".to_string());

        let report = eyre!(
            "{stream_name} of command did not log any matches for the given regex,\n\
             within the {timeout} command timeout",
        )
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
    fn write_to_test_logs<S>(line: S, bypass_test_capture: bool)
    where
        S: AsRef<str>,
    {
        write_to_test_logs(line, bypass_test_capture);
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
            let kill_res = self.kill(true);
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
        self.kill_and_consume_output(true)
            .expect("failure reading test process logs")
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
    /// See [`Self::output_check`] for details.
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
    /// See [`Self::any_output_line`] for details.
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
        .with_section(|| format!("{s:?}").header("Match String:"))
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
        .with_section(|| format!("{s:?}").header("Match String:"))
    }

    /// Tests if standard output matches `regex`.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn stdout_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegexSet + Debug,
    {
        let re = regex.to_regex_set().expect("regex must be valid");

        self.output_check(
            |stdout| re.is_match(stdout),
            &self.output.stdout,
            "stdout",
            "matched the given regex",
        )
        .with_section(|| format!("{regex:?}").header("Match Regex:"))
    }

    /// Tests if any lines in standard output contain `s`.
    #[instrument(skip(self))]
    pub fn stdout_line_contains(&self, s: &str) -> Result<&Self> {
        self.any_output_line_contains(s, &self.output.stdout, "stdout", "the given string")
    }

    /// Tests if any lines in standard output match `regex`.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn stdout_line_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegexSet + Debug,
    {
        let re = regex.to_regex_set().expect("regex must be valid");

        self.any_output_line(
            |line| re.is_match(line),
            &self.output.stdout,
            "stdout",
            "matched the given regex",
        )
        .with_section(|| format!("{regex:?}").header("Line Match Regex:"))
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
        .with_section(|| format!("{s:?}").header("Match String:"))
    }

    /// Tests if standard error matches `regex`.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn stderr_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegexSet + Debug,
    {
        let re = regex.to_regex_set().expect("regex must be valid");

        self.output_check(
            |stderr| re.is_match(stderr),
            &self.output.stderr,
            "stderr",
            "matched the given regex",
        )
        .with_section(|| format!("{regex:?}").header("Match Regex:"))
    }

    /// Tests if any lines in standard error contain `s`.
    #[instrument(skip(self))]
    pub fn stderr_line_contains(&self, s: &str) -> Result<&Self> {
        self.any_output_line_contains(s, &self.output.stderr, "stderr", "the given string")
    }

    /// Tests if any lines in standard error match `regex`.
    #[instrument(skip(self))]
    #[allow(clippy::unwrap_in_result)]
    pub fn stderr_line_matches<R>(&self, regex: R) -> Result<&Self>
    where
        R: ToRegexSet + Debug,
    {
        let re = regex.to_regex_set().expect("regex must be valid");

        self.any_output_line(
            |line| re.is_match(line),
            &self.output.stderr,
            "stderr",
            "matched the given regex",
        )
        .with_section(|| format!("{regex:?}").header("Line Match Regex:"))
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

    /// Takes the generic `dir` parameter out of this `TestOutput`.
    pub fn take_dir(&mut self) -> Option<T> {
        self.dir.take()
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
                let line = line.unwrap_or_else(|error| {
                    format!("failure reading test process logs: {error:?}")
                });
                let _ = writeln!(&mut stdout_buf, "{line}");
            }
        } else if let Some(child) = &mut source.child {
            if let Some(stdout) = &mut child.stdout {
                let _ = stdout.read_to_string(&mut stdout_buf);
            }
        }

        if let Some(stderr) = &mut source.stderr {
            for line in stderr {
                let line = line.unwrap_or_else(|error| {
                    format!("failure reading test process logs: {error:?}")
                });
                let _ = writeln!(&mut stderr_buf, "{line}");
            }
        } else if let Some(child) = &mut source.child {
            if let Some(stderr) = &mut child.stderr {
                let _ = stderr.read_to_string(&mut stderr_buf);
            }
        }

        self.section(stdout_buf.header(format!("{} Unread Stdout:", source.command_path)))
            .section(stderr_buf.header(format!("{} Unread Stderr:", source.command_path)))
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
        // TODO: add TestChild.command_path before Stdout and Stderr header names
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
                format!("command exited {how} with status code {code}").header("Exit Status:")
            });
        }

        #[cfg(unix)]
        if let Some(signal) = source.signal() {
            self.with_section(|| {
                format!("command terminated {how} by signal {signal}").header("Exit Status:")
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
