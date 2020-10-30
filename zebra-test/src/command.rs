use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use tracing::instrument;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    convert::Infallible as NoDir,
    fmt::Write as _,
    io::BufRead,
    io::{BufReader, Lines, Read},
    path::Path,
    process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

/// Runs a command
pub fn test_cmd(command_path: &str, tempdir: &Path) -> Result<Command> {
    let mut cmd = Command::new(command_path);
    cmd.current_dir(tempdir);

    Ok(cmd)
}

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

        Ok(TestStatus { status, cmd })
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
            bypass_test_stdout: false,
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
    pub deadline: Option<Instant>,
    bypass_test_stdout: bool,
}

impl<T> TestChild<T> {
    /// Kill the child process.
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

    /// Set a timeout for `expect_stdout`.
    ///
    /// Does not apply to `wait_with_output`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Some(Instant::now() + timeout);
        self
    }

    /// Configures testrunner to forward stdout to the true stdout rather than
    /// fakestdout used by cargo tests.
    pub fn bypass_test_stdout(mut self, cond: bool) -> Self {
        self.bypass_test_stdout = cond;
        self
    }

    /// Checks each line of the child's stdout against `regex`, and returns matching lines.
    ///
    /// Kills the child after the configured timeout has elapsed.
    /// Note: the timeout is only checked after each line.
    #[instrument(skip(self))]
    pub fn expect_stdout(&mut self, regex: &str) -> Result<&mut Self> {
        if self.stdout.is_none() {
            self.stdout = self
                .child
                .stdout
                .take()
                .map(BufReader::new)
                .map(BufRead::lines)
        }

        let re = regex::Regex::new(regex).expect("regex must be valid");
        let mut lines = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout");

        while !self.past_deadline() && self.is_running() {
            let line = if let Some(line) = lines.next() {
                line?
            } else {
                break;
            };

            // since we're about to discard this line write it to stdout so our
            // test runner can capture it and display if the test fails, may
            // cause weird reordering for stdout / stderr
            if !self.bypass_test_stdout {
                println!("{}", line);
            } else {
                use std::io::Write;
                #[allow(clippy::explicit_write)]
                writeln!(std::io::stdout(), "{}", line).unwrap();
            }

            if re.is_match(&line) {
                self.stdout = Some(lines);
                return Ok(self);
            }
        }

        if self.is_running() {
            // If the process exits between is_running and kill, we will see
            // spurious errors here. If that happens, ignore "no such process"
            // errors from kill.
            self.kill()?;
        }

        let report = eyre!("stdout of command did not contain any matches for the given regex")
            .context_from(self)
            .with_section(|| format!("{:?}", regex).header("Match Regex:"));

        Err(report)
    }

    fn past_deadline(&self) -> bool {
        self.deadline
            .map(|deadline| Instant::now() > deadline)
            .unwrap_or(false)
    }

    fn is_running(&mut self) -> bool {
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

    #[instrument(skip(self))]
    pub fn stdout_contains(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        for line in stdout.lines() {
            if re.is_match(line) {
                return Ok(self);
            }
        }

        Err(eyre!(
            "stdout of command did not contain any matches for the given regex"
        ))
        .context_from(self)
        .with_section(|| format!("{:?}", regex).header("Match Regex:"))
    }

    #[instrument(skip(self))]
    pub fn stdout_equals(&self, s: &str) -> Result<&Self> {
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        if stdout == s {
            return Ok(self);
        }

        Err(eyre!("stdout of command is not equal the given string"))
            .context_from(self)
            .with_section(|| format!("{:?}", s).header("Match String:"))
    }

    #[instrument(skip(self))]
    pub fn stdout_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        if re.is_match(&stdout) {
            return Ok(self);
        }

        Err(eyre!("stdout of command is not equal to the given regex"))
            .context_from(self)
            .with_section(|| format!("{:?}", regex).header("Match Regex:"))
    }

    /// Returns Ok if the program was killed, Err(Report) if exit was by another
    /// reason.
    pub fn assert_was_killed(&self) -> Result<()> {
        if self.was_killed() {
            Err(eyre!("command was killed")).context_from(self)?
        }

        Ok(())
    }

    /// Returns Ok if the program was not killed, Err(Report) if exit was by
    /// another reason.
    pub fn assert_was_not_killed(&self) -> Result<()> {
        if !self.was_killed() {
            Err(eyre!("command wasn't killed")).context_from(self)?
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn was_killed(&self) -> bool {
        self.output.status.code() != Some(1)
    }

    #[cfg(unix)]
    fn was_killed(&self) -> bool {
        self.output.status.signal() != Some(9)
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

        if let Some(stderr) = &mut source.child.stderr {
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
