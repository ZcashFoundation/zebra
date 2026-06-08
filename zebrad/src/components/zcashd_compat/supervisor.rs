use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::Stdio,
};

use color_eyre::eyre::{eyre, Report};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::{debug, error, info, warn};

use zebra_chain::parameters::NetworkKind;

use crate::components::zcashd_compat::Config;

/// The full configuration used by the zcashd-compat supervisor task.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// Path to the `zcashd` binary.
    pub zcashd_path: PathBuf,
    /// Datadir for `zcashd`.
    pub zcashd_datadir: PathBuf,
    /// RPC URL passed to `-unityzebra`.
    pub rpc_url: String,
    /// Cookie file path passed to `-unityzebracookiefile`.
    pub cookie_path: PathBuf,
    /// Any extra user-provided arguments.
    pub extra_args: Vec<String>,
    /// Active Zebra network kind.
    pub network: NetworkKind,
    /// Delay before first spawn.
    pub startup_delay: std::time::Duration,
    /// Restart backoff.
    pub restart_backoff: std::time::Duration,
    /// Restart limit.
    pub max_restarts: u32,
    /// Grace period after SIGTERM.
    pub shutdown_grace_period: std::time::Duration,
}

impl SupervisorConfig {
    /// Builds a runtime supervisor config from `zebrad` and `[zcashd_compat]` settings.
    pub fn new(
        zcashd_compat: &Config,
        state_cache_dir: &Path,
        network: NetworkKind,
        rpc_url: String,
        cookie_path: PathBuf,
    ) -> Self {
        Self {
            zcashd_path: zcashd_compat.zcashd_path.clone(),
            zcashd_datadir: zcashd_compat
                .zcashd_datadir
                .clone()
                .unwrap_or_else(|| state_cache_dir.join("zcashd-compat-zcashd")),
            rpc_url,
            cookie_path,
            extra_args: zcashd_compat.zcashd_extra_args.clone(),
            network,
            startup_delay: zcashd_compat.startup_delay,
            restart_backoff: zcashd_compat.restart_backoff,
            max_restarts: zcashd_compat.max_restarts,
            shutdown_grace_period: zcashd_compat.shutdown_grace_period,
        }
    }

    /// Builds the zcashd command-line arguments.
    pub fn command_args(&self) -> Vec<String> {
        let mut args = vec![
            "-unity".to_string(),
            format!("-unityzebra={}", self.rpc_url),
            format!(
                "-unityzebracookiefile={}",
                self.cookie_path.to_string_lossy()
            ),
            format!("-datadir={}", self.zcashd_datadir.to_string_lossy()),
        ];

        match self.network {
            NetworkKind::Mainnet => {}
            NetworkKind::Testnet => args.push("-testnet".to_string()),
            NetworkKind::Regtest => args.push("-regtest".to_string()),
        }

        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Runs the zcashd-compat zcashd supervisor until shutdown.
///
/// The supervisor keeps restarting `zcashd` exits that happen before Zebra
/// shutdown, up to `max_restarts`.
///
/// # Errors
///
/// Returns an error if spawning `zcashd` fails, if shutdown handling fails, or
/// if the restart limit is exceeded.
pub async fn run(
    config: SupervisorConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Report> {
    if wait_for_delay_or_shutdown(config.startup_delay, &mut shutdown_rx).await {
        info!("zcashd-compat supervisor received shutdown during startup delay");
        return Ok(());
    }

    let mut restart_count = 0u32;

    loop {
        if *shutdown_rx.borrow() {
            info!("zcashd-compat supervisor received shutdown before spawn");
            return Ok(());
        }

        let mut child = spawn_zcashd(&config)?;
        info!(
            path = %config.zcashd_path.display(),
            datadir = %config.zcashd_datadir.display(),
            rpc_url = %config.rpc_url,
            cookie = %config.cookie_path.display(),
            "started zcashd-compat zcashd child"
        );

        let child_result = wait_for_child_or_shutdown(&mut child, &mut shutdown_rx).await;
        match child_result {
            ChildOutcome::ShutdownRequested => {
                terminate_child(&mut child, config.shutdown_grace_period).await?;
                info!("zcashd-compat zcashd child stopped on shutdown");
                return Ok(());
            }
            ChildOutcome::Exited(status) => {
                restart_count = restart_count.saturating_add(1);
                warn!(
                    ?status,
                    restart_count,
                    max_restarts = config.max_restarts,
                    "zcashd-compat zcashd child exited before shutdown, restarting"
                );

                if restart_count > config.max_restarts {
                    return Err(eyre!(
                        "zcashd-compat zcashd child exceeded restart limit: {}",
                        config.max_restarts
                    ));
                }

                if wait_for_delay_or_shutdown(config.restart_backoff, &mut shutdown_rx).await {
                    info!("zcashd-compat supervisor received shutdown during restart backoff");
                    return Ok(());
                }
            }
        }
    }
}

/// Spawns `zcashd` with zcashd-compat arguments and connects child output streams.
///
/// # Errors
///
/// Returns an error if the child process cannot be spawned.
fn spawn_zcashd(config: &SupervisorConfig) -> Result<Child, Report> {
    let args = config.command_args();

    let mut command = Command::new(&config.zcashd_path);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|err| eyre!("failed to spawn zcashd-compat zcashd process: {err}"))?;

    if let Some(stdout) = child.stdout.take() {
        spawn_log_task(stdout, "stdout");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_task(stderr, "stderr");
    }

    Ok(child)
}

/// Forwards a child output stream into Zebra logs under `zcashd_compat.zcashd`.
fn spawn_log_task<T>(stream: T, stream_name: &'static str) -> JoinHandle<()>
where
    T: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();

        while let Ok(Some(line)) = reader.next_line().await {
            let line = sanitize_child_log_line(&line);

            if stream_name == "stderr" {
                error!(target: "zcashd_compat.zcashd", stream = stream_name, "{line}");
            } else {
                info!(target: "zcashd_compat.zcashd", stream = stream_name, "{line}");
            }
        }
    })
}

/// Returns a sanitized log line with ANSI escape/control noise removed.
fn sanitize_child_log_line(line: &str) -> Cow<'_, str> {
    let has_escape_or_control = line
        .bytes()
        .any(|byte| byte == 0x1b || (byte.is_ascii_control() && byte != b'\t'));

    if !has_escape_or_control {
        return Cow::Borrowed(line);
    }

    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    enum ParseState {
        Normal,
        Escape,
        Csi,
        Osc,
    }

    let mut state = ParseState::Normal;

    while let Some(ch) = chars.next() {
        match state {
            ParseState::Normal => {
                if ch == '\u{1b}' {
                    state = ParseState::Escape;
                } else if !(ch.is_control() && ch != '\t') {
                    output.push(ch);
                }
            }
            ParseState::Escape => {
                state = match ch {
                    '[' => ParseState::Csi,
                    ']' => ParseState::Osc,
                    _ => ParseState::Normal,
                };
            }
            ParseState::Csi => {
                if ('@'..='~').contains(&ch) {
                    state = ParseState::Normal;
                }
            }
            ParseState::Osc => {
                if ch == '\u{7}' {
                    state = ParseState::Normal;
                } else if ch == '\u{1b}' && chars.peek() == Some(&'\\') {
                    let _ = chars.next();
                    state = ParseState::Normal;
                }
            }
        }
    }

    Cow::Owned(output)
}

enum ChildOutcome {
    ShutdownRequested,
    Exited(std::process::ExitStatus),
}

/// Waits for `delay` to elapse, returning `true` if shutdown is requested first.
async fn wait_for_delay_or_shutdown(
    delay: std::time::Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> bool {
    if *shutdown_rx.borrow() {
        return true;
    }

    if delay == std::time::Duration::ZERO {
        return false;
    }

    let delay = sleep(delay);
    tokio::pin!(delay);

    loop {
        tokio::select! {
            () = &mut delay => return false,
            changed = shutdown_rx.changed() => {
                if changed.is_err() {
                    debug!("zcashd-compat shutdown sender dropped");
                    return true;
                }

                if *shutdown_rx.borrow_and_update() {
                    return true;
                }
            }
        }
    }
}

/// Waits until either a shutdown request arrives or the child exits.
///
/// If waiting on the child fails, returns a synthesized non-zero exit status so
/// the supervisor can apply its restart policy.
async fn wait_for_child_or_shutdown(
    child: &mut Child,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> ChildOutcome {
    tokio::select! {
        changed = shutdown_rx.changed() => {
            if changed.is_err() {
                debug!("zcashd-compat shutdown sender dropped");
            }
            ChildOutcome::ShutdownRequested
        }
        exited = child.wait() => {
            match exited {
                Ok(status) => ChildOutcome::Exited(status),
                Err(error) => {
                    error!(?error, "failed waiting on zcashd-compat zcashd child");
                    ChildOutcome::Exited(exit_status_failure())
                }
            }
        }
    }
}

/// Attempts graceful termination of the zcashd-compat child process.
///
/// On Unix, this sends SIGTERM first. If the process has not exited after
/// `shutdown_grace_period`, it is force-killed.
///
/// # Errors
///
/// Returns an error if waiting for process termination fails.
async fn terminate_child(
    child: &mut Child,
    shutdown_grace_period: std::time::Duration,
) -> Result<(), Report> {
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{kill, Signal::SIGTERM},
            unistd::Pid,
        };

        if let Some(id) = child.id() {
            let pid = id as i32;
            let _ = kill(Pid::from_raw(pid), SIGTERM);
        }
    }

    let wait_result = timeout(shutdown_grace_period, child.wait()).await;
    match wait_result {
        Ok(Ok(_status)) => Ok(()),
        Ok(Err(error)) => Err(eyre!(
            "failed waiting for zcashd-compat zcashd shutdown: {error}"
        )),
        Err(_timeout) => {
            warn!("zcashd-compat zcashd did not exit after SIGTERM, sending kill");
            child
                .start_kill()
                .map_err(|err| eyre!("failed to kill zcashd-compat zcashd child: {err}"))?;
            let _ = child.wait().await;
            Ok(())
        }
    }
}

/// Returns a synthetic non-zero exit status for wait errors.
fn exit_status_failure() -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }

    #[cfg(not(any(unix, windows)))]
    {
        panic!("unsupported platform for zcashd-compat exit status synthesis")
    }
}

/// Returns `true` if the given command path is resolvable as an executable.
///
/// Paths containing separators are validated directly, while bare command names
/// are searched in `PATH`.
pub fn is_command_resolvable(path: &Path) -> bool {
    if path.components().count() > 1 {
        return is_executable(path);
    }

    std::env::var_os("PATH").is_some_and(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(path))
            .any(|candidate| candidate.exists() && is_executable(&candidate))
    })
}

/// Returns `true` when `path` points to an executable regular file.
///
/// On Unix this checks execute mode bits. On non-Unix targets this checks
/// common executable filename extensions.
fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| (metadata.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        use std::ffi::OsStr;

        let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
        return matches!(
            extension.to_ascii_lowercase().as_str(),
            "exe" | "cmd" | "bat" | "com"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use tokio::sync::watch;
    use zebra_chain::parameters::NetworkKind;

    use super::{wait_for_delay_or_shutdown, SupervisorConfig};

    #[test]
    fn command_args_include_zcashd_compat_flags() {
        let config = SupervisorConfig {
            zcashd_path: PathBuf::from("zcashd"),
            zcashd_datadir: PathBuf::from("/tmp/zcashd-compat-datadir"),
            rpc_url: "http://127.0.0.1:8232".to_string(),
            cookie_path: PathBuf::from("/tmp/.cookie"),
            extra_args: vec!["-printtoconsole".to_string()],
            network: NetworkKind::Regtest,
            startup_delay: std::time::Duration::from_secs(1),
            restart_backoff: std::time::Duration::from_secs(2),
            max_restarts: 3,
            shutdown_grace_period: std::time::Duration::from_secs(10),
        };

        let args = config.command_args();

        assert!(args.contains(&"-unity".to_string()));
        assert!(args.contains(&"-regtest".to_string()));
        assert!(args
            .iter()
            .any(|a| a.starts_with("-unityzebra=http://127.0.0.1:8232")));
        assert!(args
            .iter()
            .any(|a| a.starts_with("-unityzebracookiefile=/tmp/.cookie")));
        assert!(args.contains(&"-printtoconsole".to_string()));
    }

    #[tokio::test]
    async fn delay_wait_returns_on_shutdown_request() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let wait = tokio::spawn(async move {
            wait_for_delay_or_shutdown(Duration::from_secs(60), &mut shutdown_rx).await
        });

        shutdown_tx
            .send(true)
            .expect("shutdown receiver exists because wait task owns it");

        let was_shutdown = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("interruptible delay should complete promptly")
            .expect("wait task should not panic");

        assert!(was_shutdown);
    }

    #[tokio::test]
    async fn delay_wait_returns_on_dropped_shutdown_sender() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let wait = tokio::spawn(async move {
            wait_for_delay_or_shutdown(Duration::from_secs(60), &mut shutdown_rx).await
        });

        drop(shutdown_tx);

        let was_shutdown = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("interruptible delay should complete promptly")
            .expect("wait task should not panic");

        assert!(was_shutdown);
    }

    #[test]
    fn sanitize_child_log_line_strips_ansi_csi_sequences() {
        let line = "\u{1b}[32mINFO\u{1b}[0m ProcessNewTrustedBlockBatch";
        let sanitized = super::sanitize_child_log_line(line);

        assert_eq!(sanitized, "INFO ProcessNewTrustedBlockBatch");
    }

    #[test]
    fn sanitize_child_log_line_removes_control_chars() {
        let line = "good\u{0}text\u{8}\tkeeps-tab";
        let sanitized = super::sanitize_child_log_line(line);

        assert_eq!(sanitized, "goodtext\tkeeps-tab");
    }

    #[test]
    fn sanitize_child_log_line_keeps_clean_lines_unchanged() {
        let line = "UpdateTip: new best hash=abc height=42";
        let sanitized = super::sanitize_child_log_line(line);

        assert_eq!(sanitized, line);
    }
}
