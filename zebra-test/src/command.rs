//! Launching test commands for Zebra integration and acceptance tests.

use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use tracing::instrument;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    convert::Infallible as NoDir,
    fmt::{self, Write as _},
    io::BufRead,
    io::{BufReader, Lines, Read},
    path::Path,
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

/// Runs a command
pub fn test_cmd(command_path: &str, tempdir: &Path) -> Result<Command> {
    let mut cmd = Command::new(command_path);
    cmd.current_dir(tempdir);

    Ok(cmd)
}

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
            child,
            cmd,
            dir,
            deadline: None,
            stdout: None,
            stderr: None,
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

#[derive(Debug)]
pub struct TestStatus {
    pub cmd: String,
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

#[derive(Debug)]
pub struct TestChild<T> {
    pub dir: T,
    pub cmd: String,
    pub child: Child,
    pub stdout: Option<Lines<BufReader<ChildStdout>>>,
    pub stderr: Option<Lines<BufReader<ChildStderr>>>,
    pub deadline: Option<Instant>,
    bypass_test_capture: bool,
}

impl<T> TestChild<T> {
    /// Kill the child process.
    ///
    /// ## BUGS
    ///
    /// On Windows (and possibly macOS), this function can return `Ok` for
    /// processes that have panicked. See #1781.
    #[spandoc::spandoc]
    pub fn kill(&mut self) -> Result<()> {
        /// SPANDOC: Killing child process
        self.child.kill().context_from(self)?;

        Ok(())
    }

    /// Waits for the child process to exit, then returns its output.
    ///
    /// Ignores any configured timeouts.
    #[spandoc::spandoc]
    pub fn wait_with_output(self) -> Result<TestOutput<T>> {
        /// SPANDOC: waiting for command to exit
        let output = self.child.wait_with_output().with_section({
            let cmd = self.cmd.clone();
            || cmd.header("Command:")
        })?;

        Ok(TestOutput {
            output,
            cmd: self.cmd,
            dir: Some(self.dir),
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

    /// Checks each line of the child's stdout against `regex`, and returns Ok
    /// if a line matches.
    ///
    /// Kills the child after the configured timeout has elapsed.
    /// See `expect_line_matching` for details.
    #[instrument(skip(self))]
    pub fn expect_stdout_line_matches(&mut self, regex: &str) -> Result<&mut Self> {
        if self.stdout.is_none() {
            self.stdout = self
                .child
                .stdout
                .take()
                .map(BufReader::new)
                .map(BufRead::lines)
        }

        let mut lines = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout_line_matches");

        match self.expect_line_matching(&mut lines, regex, "stdout") {
            Ok(()) => {
                self.stdout = Some(lines);
                Ok(self)
            }
            Err(report) => Err(report),
        }
    }

    /// Checks each line of the child's stderr against `regex`, and returns Ok
    /// if a line matches.
    ///
    /// Kills the child after the configured timeout has elapsed.
    /// See `expect_line_matching` for details.
    #[instrument(skip(self))]
    pub fn expect_stderr_line_matches(&mut self, regex: &str) -> Result<&mut Self> {
        if self.stderr.is_none() {
            self.stderr = self
                .child
                .stderr
                .take()
                .map(BufReader::new)
                .map(BufRead::lines)
        }

        let mut lines = self
            .stderr
            .take()
            .expect("child must capture stderr to call expect_stderr_line_matches");

        match self.expect_line_matching(&mut lines, regex, "stderr") {
            Ok(()) => {
                self.stderr = Some(lines);
                Ok(self)
            }
            Err(report) => Err(report),
        }
    }

    /// Checks each line in `lines` against `regex`, and returns Ok if a line
    /// matches. Uses `stream_name` as the name for `lines` in error reports.
    ///
    /// Kills the child after the configured timeout has elapsed.
    /// Note: the timeout is only checked after each full line is received from
    /// the child.
    #[instrument(skip(self, lines))]
    pub fn expect_line_matching<L>(
        &mut self,
        lines: &mut L,
        regex: &str,
        stream_name: &str,
    ) -> Result<()>
    where
        L: Iterator<Item = std::io::Result<String>>,
    {
        let re = regex::Regex::new(regex).expect("regex must be valid");
        while !self.past_deadline() && self.is_running() {
            let line = if let Some(line) = lines.next() {
                line?
            } else {
                break;
            };

            // Since we're about to discard this line write it to stdout, so it
            // can be preserved. May cause weird reordering for stdout / stderr.
            // Uses stdout even if the original lines were from stderr.
            if self.bypass_test_capture {
                // send lines to the terminal (or process stdout file redirect)
                use std::io::Write;
                #[allow(clippy::explicit_write)]
                writeln!(std::io::stdout(), "{}", line).unwrap();
            } else {
                // if the test fails, the test runner captures and displays it
                println!("{}", line);
            }

            if re.is_match(&line) {
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
        .with_section(|| format!("{:?}", regex).header("Match Regex:"));

        Err(report)
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
    /// ## BUGS
    ///
    /// On Windows and macOS, this function can return `true` for processes that
    /// have panicked. See #1781.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

pub struct TestOutput<T> {
    #[allow(dead_code)]
    // this just keeps the test dir around from `TestChild` so it doesnt get
    // deleted during `wait_with_output`
    dir: Option<T>,
    pub cmd: String,
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
    pub fn stdout_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;

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
    pub fn stdout_line_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;

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
    pub fn stderr_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;

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
    pub fn stderr_line_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;

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

    fn context_from(mut self, source: &mut TestChild<T>) -> Self::Return {
        self = self.section(source.cmd.clone().header("Command:"));

        if let Ok(Some(status)) = source.child.try_wait() {
            self = self.context_from(&status);
        }

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        if let Some(stdout) = &mut source.stdout {
            for line in stdout {
                let line = if let Ok(line) = line { line } else { break };
                let _ = writeln!(&mut stdout_buf, "{}", line);
            }
        } else if let Some(stdout) = &mut source.child.stdout {
            let _ = stdout.read_to_string(&mut stdout_buf);
        }

        if let Some(stderr) = &mut source.stderr {
            for line in stderr {
                let line = if let Ok(line) = line { line } else { break };
                let _ = writeln!(&mut stderr_buf, "{}", line);
            }
        } else if let Some(stderr) = &mut source.child.stderr {
            let _ = stderr.read_to_string(&mut stderr_buf);
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
