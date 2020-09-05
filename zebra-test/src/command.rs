use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use tracing::instrument;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::{
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Command, ExitStatus, Output},
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
    fn output2(&mut self) -> Result<TestOutput, Report>;

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
    fn output2(&mut self) -> Result<TestOutput, Report> {
        let output = self.output();

        let output = output
            .wrap_err("failed to execute process")
            .with_section(|| format!("{:?}", self).header("Command:"))?;

        Ok(TestOutput {
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
        })
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
    dir: T,
    pub cmd: String,
    pub child: Child,
    pub stdout: Option<BufReader<ChildStdout>>,
    pub deadline: Option<Instant>,
}

impl<T> TestChild<T> {
    #[spandoc::spandoc]
    pub fn kill(&mut self) -> Result<()> {
        /// SPANDOC: Killing child process
        self.child.kill().context_from(self)?;

        Ok(())
    }

    #[spandoc::spandoc]
    pub fn wait_with_output(self) -> Result<TestOutput> {
        /// SPANDOC: waiting for command to exit
        let output = self.child.wait_with_output().with_section({
            let cmd = self.cmd.clone();
            || cmd.header("Command:")
        })?;

        Ok(TestOutput {
            output,
            cmd: self.cmd,
        })
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Some(Instant::now() + timeout);
        self
    }

    // #[instrument(skip(self))]
    pub fn expect_stdout(&mut self, regex: &str) -> Result<&mut Self> {
        if self.stdout.is_none() {
            self.stdout = self.child.stdout.take().map(BufReader::new)
        }

        let mut reader = self
            .stdout
            .take()
            .expect("child must capture stdout to call expect_stdout");

        let re = regex::Regex::new(regex)?;
        // using bufread here can cause data to be dropped between calls to
        // `expect_stdout`, but I think it won't in practice so long as we only
        // call `read_line` instead of `lines`.

        let mut line = String::new();

        while !self.past_deadline() && self.is_running() && reader.read_line(&mut line)? > 0 {
            print!("{}", line);
            if re.is_match(&line) {
                self.stdout = Some(reader);
                return Ok(self);
            }
        }

        if self.past_deadline() && !self.is_running() {
            self.kill()?;
        }

        Err(eyre!(
            "stdout of command did not contain any matches for the given regex"
        ))
        .context_from(self)
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

pub struct TestOutput {
    pub cmd: String,
    pub output: Output,
}

impl TestOutput {
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
    }

    #[instrument(skip(self))]
    pub fn stdout_equals(&self, s: &str) -> Result<&Self> {
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        if stdout == s {
            return Ok(self);
        }

        Err(eyre!("stdout of command is not equal the given string")).context_from(self)
    }

    #[instrument(skip(self))]
    pub fn stdout_matches(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        if re.is_match(&stdout) {
            return Ok(self);
        }

        Err(eyre!("stdout of command is not equal to the given regex")).context_from(self)
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

    fn context_from(self, source: &S) -> Self::Return;
}

impl<C, T, E> ContextFrom<C> for Result<T, E>
where
    E: Into<Report>,
    Report: ContextFrom<C, Return = Report>,
{
    type Return = Result<T, Report>;

    fn context_from(self, source: &C) -> Self::Return {
        self.map_err(|e| e.into())
            .map_err(|report| report.context_from(source))
    }
}

impl ContextFrom<TestStatus> for Report {
    type Return = Report;

    fn context_from(self, source: &TestStatus) -> Self::Return {
        let command = || source.cmd.clone().header("Command:");

        self.with_section(command).context_from(&source.status)
    }
}

impl<T> ContextFrom<TestChild<T>> for Report {
    type Return = Report;

    fn context_from(self, source: &TestChild<T>) -> Self::Return {
        let command = || source.cmd.clone().header("Command:");

        self.with_section(command)
    }
}

impl ContextFrom<TestOutput> for Report {
    type Return = Report;

    fn context_from(self, source: &TestOutput) -> Self::Return {
        self.with_section(|| source.cmd.clone().header("Command:"))
            .context_from(&source.output)
    }
}

impl ContextFrom<Output> for Report {
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

impl ContextFrom<ExitStatus> for Report {
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
