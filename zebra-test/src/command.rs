use color_eyre::{
    eyre::{eyre, Context, Report, Result},
    Help, SectionExt,
};
use std::process::{Child, Command, ExitStatus, Output};
use std::{fs, io::Write, path::PathBuf};
use tempdir::TempDir;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

/// Runs a command in already existing temp dir or create new if `tempdir_path` is `None`
pub fn test_cmd(command_path: &str, tempdir_path: Option<&str>) -> Result<(Command, ZebraTestDir)> {
    let dir = match tempdir_path {
        Some(t) => ZebraTestDir::new_in(t, command_path),
        None => {
            let dir = ZebraTestDir::new(command_path);

            // Make temp dir to be cache_dir
            let cache_dir = dir.path().join("state");
            fs::create_dir(&cache_dir)?;
            fs::File::create(dir.path().join("zebrad.toml"))?.write_all(
                format!(
                    "[state]\ncache_dir = '{}'",
                    cache_dir
                        .into_os_string()
                        .into_string()
                        .map_err(|_| eyre!("tmp dir path cannot be encoded as UTF8"))?
                )
                .as_bytes(),
            )?;
            dir
        }
    };

    let mut cmd = Command::new(command_path);
    cmd.current_dir(dir.path());

    Ok((cmd, dir))
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
        let exit_code = || {
            if let Some(code) = status.code() {
                format!("Exit Code: {}", code)
            } else {
                "Exit Code: None".into()
            }
        };

        Err(eyre!("command exited unsuccessfully"))
            .with_section(|| cmd.to_string().header("Command:"))
            .with_section(exit_code)?;
    }

    Ok(())
}

fn assert_failure(status: &ExitStatus, cmd: &str) -> Result<()> {
    if status.success() {
        let exit_code = || {
            if let Some(code) = status.code() {
                format!("Exit Code: {}", code)
            } else {
                "Exit Code: None".into()
            }
        };

        Err(eyre!("command unexpectedly exited successfully"))
            .with_section(|| cmd.to_string().header("Command:"))
            .with_section(exit_code)?;
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
        self.child
            .kill()
            .with_section(|| self.cmd.clone().header("Child Process:"))?;

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
        let output = &self.output;

        assert_success(&self.output.status, &self.cmd)
            .with_section(|| {
                String::from_utf8_lossy(output.stdout.as_slice())
                    .to_string()
                    .header("Stdout:")
            })
            .with_section(|| {
                String::from_utf8_lossy(output.stderr.as_slice())
                    .to_string()
                    .header("Stderr:")
            })?;

        Ok(self)
    }

    pub fn assert_failure(self) -> Result<Self> {
        let output = &self.output;

        assert_failure(&self.output.status, &self.cmd)
            .with_section(|| {
                String::from_utf8_lossy(output.stdout.as_slice())
                    .to_string()
                    .header("Stdout:")
            })
            .with_section(|| {
                String::from_utf8_lossy(output.stderr.as_slice())
                    .to_string()
                    .header("Stderr:")
            })?;

        Ok(self)
    }

    pub fn stdout_contains(&self, regex: &str) -> Result<&Self> {
        let re = regex::Regex::new(regex)?;
        let stdout = String::from_utf8_lossy(self.output.stdout.as_slice());

        for line in stdout.lines() {
            if re.is_match(line) {
                return Ok(self);
            }
        }

        let command = || self.cmd.clone().header("Command:");
        let stdout = || stdout.into_owned().header("Stdout:");

        Err(eyre!(
            "stdout of command did not contain any matches for the given regex"
        ))
        .with_section(command)
        .with_section(stdout)
    }

    /// Returns true if the program was killed, false if exit was by another reason.
    pub fn was_killed(&self) -> bool {
        #[cfg(unix)]
        return self.output.status.signal() == Some(9);

        #[cfg(not(unix))]
        return self.output.status.code() == Some(1);
    }
}

/// Provide functions to manage a TempDir
pub struct ZebraTestDir {
    pub tempdir: Option<TempDir>,
}

impl ZebraTestDir {
    pub fn new(prefix: &str) -> Self {
        Self {
            tempdir: Some(TempDir::new(prefix).unwrap()),
        }
    }
    pub fn new_in(dir: &str, prefix: &str) -> Self {
        Self {
            tempdir: Some(TempDir::new_in(dir, prefix).unwrap()),
        }
    }
    pub fn path(&self) -> PathBuf {
        PathBuf::from(self.tempdir.as_ref().unwrap().path())
    }
}
