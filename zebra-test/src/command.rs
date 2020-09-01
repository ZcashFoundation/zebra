use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

/// Runs a command
pub fn test_cmd(command_path: &str, tempdir: &PathBuf) -> Result<Command> {
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
    fn spawn2(&mut self) -> Result<TestChild, Report>;
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
    fn spawn2(&mut self) -> Result<TestChild, Report> {
        let cmd = format!("{:?}", self);
        let child = self.spawn();

        let child = child
            .wrap_err("failed to execute process")
            .with_section(|| cmd.clone().header("Command:"))?;

        Ok(TestChild { child, cmd })
    }
}

#[derive(Debug)]
pub struct TestStatus {
    pub cmd: String,
    pub status: ExitStatus,
}

impl TestStatus {
    pub fn assert_success(self) -> Result<Self> {
        assert_success(&self.status, &self.cmd)?;

        Ok(self)
    }

    pub fn assert_failure(self) -> Result<Self> {
        assert_failure(&self.status, &self.cmd)?;

        Ok(self)
    }
}

fn assert_success(status: &ExitStatus, cmd: &str) -> Result<()> {
    if !status.success() {
        Err(eyre!("command exited unsuccessfully"))
            .with_section(|| cmd.to_string().header("Command:"))
            .context_from(status)?;
    }

    Ok(())
}

fn assert_failure(status: &ExitStatus, cmd: &str) -> Result<()> {
    if status.success() {
        Err(eyre!("command unexpectedly exited successfully"))
            .with_section(|| cmd.to_string().header("Command:"))
            .context_from(status)?;
    }

    Ok(())
}

#[derive(Debug)]
pub struct TestChild {
    pub cmd: String,
    pub child: Child,
}

impl TestChild {
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
}

pub struct TestOutput {
    pub cmd: String,
    pub output: Output,
}

impl TestOutput {
    pub fn assert_success(self) -> Result<Self> {
        assert_success(&self.output.status, &self.cmd).context_from(&self)?;

        Ok(self)
    }

    pub fn assert_failure(self) -> Result<Self> {
        assert_failure(&self.output.status, &self.cmd).context_from(&self)?;

        Ok(self)
    }

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

    pub fn stdout_equals(&self, s: &str) -> Result<&Self> {
        let stdout = String::from_utf8_lossy(&self.output.stdout);

        if stdout == s {
            return Ok(self);
        }

        Err(eyre!("stdout of command is not equal the given string")).context_from(self)
    }

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
trait Contextualize<S> {
    type WithContext;

    fn context_from(self, source: &S) -> Self::WithContext;
}

impl<T, E> Contextualize<TestChild> for Result<T, E>
where
    E: Into<Report>,
{
    type WithContext = Result<T, Report>;

    fn context_from(self, source: &TestChild) -> Self::WithContext {
        let with_report = self.map_err(|e| e.into());

        let command = || source.cmd.clone().header("Command:");
        let child = || format!("{:?}", source.child).header("Child Process:");

        with_report.with_section(command).with_section(child)
    }
}

impl<T, E> Contextualize<ExitStatus> for Result<T, E>
where
    E: Into<Report>,
{
    type WithContext = Result<T, Report>;

    fn context_from(self, source: &ExitStatus) -> Self::WithContext {
        let with_report = self.map_err(|e| e.into());

        let how = if source.success() {
            "successfully"
        } else {
            "unsuccessfully"
        };

        if let Some(code) = source.code() {
            with_report.with_section(|| {
                format!("command exited {} with status code {}", how, code).header("Exit Status:")
            })
        } else {
            with_report
                .with_section(|| {
                    format!("command exited {} without a status code", how).header("Exit Status:")
                })
                .note("processes exit without a status code on unix if terminated by a signal")
        }
    }
}

impl<T, E> Contextualize<Output> for Result<T, E>
where
    E: Into<Report>,
{
    type WithContext = Result<T, Report>;

    fn context_from(self, source: &Output) -> Self::WithContext {
        let with_report = self.map_err(|e| e.into());

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

        with_report
            .with_section(stdout)
            .with_section(stderr)
            .context_from(&source.status)
    }
}

impl<T, E> Contextualize<TestOutput> for Result<T, E>
where
    E: Into<Report>,
{
    type WithContext = Result<T, Report>;

    fn context_from(self, source: &TestOutput) -> Self::WithContext {
        let with_report = self.context_from(&source.output);

        let command = || source.cmd.clone().header("Command:");

        with_report.with_section(command)
    }
}
