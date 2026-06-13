use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
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

use super::{effective_zcashd_datadir, ensure_zcashd_datadir, resolve_zcashd_datadir_path, Config};

const SUPERVISOR_ACTIVE_METRIC: &str = "zcashd_compat.supervisor.active";
const SUPERVISOR_DISABLED_METRIC: &str = "zcashd_compat.supervisor.disabled";
const SUPERVISOR_EXHAUSTED_METRIC: &str = "zcashd_compat.supervisor.exhausted";

/// The full configuration used by the zcashd-compat supervisor task.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// Path to the `zcashd` binary.
    pub zcashd_path: PathBuf,
    /// Datadir for `zcashd`.
    pub zcashd_datadir: PathBuf,
    /// RPC URL passed to `-zebra-compat-url`.
    pub rpc_url: String,
    /// Cookie file path passed to `-zebra-compat-cookiefile`.
    pub cookie_path: PathBuf,
    /// Whether supervised zcashd should authenticate to Zebra using the cookie file.
    pub enable_cookie_auth: bool,
    /// Optional CA file passed to zcashd for Zebra TLS verification.
    pub tls_ca_file: Option<PathBuf>,
    /// Zebra RPC response body limit passed to zcashd for startup validation.
    pub zebra_rpc_max_response_body_size: usize,
    /// Any extra user-provided arguments.
    pub extra_args: Vec<String>,
    /// Active Zebra network kind.
    pub network: NetworkKind,
    /// Delay before first spawn.
    pub startup_delay: std::time::Duration,
    /// Restart backoff.
    pub restart_backoff: Duration,
    /// Maximum restart backoff.
    pub restart_backoff_max: Duration,
    /// Child uptime that resets the consecutive restart count.
    pub restart_reset_after: Duration,
    /// Grace period after SIGTERM.
    pub shutdown_grace_period: Duration,
}

impl SupervisorConfig {
    /// Builds a runtime supervisor config from `zebrad` and `[zcashd_compat]` settings.
    pub fn new(
        zcashd_compat: &Config,
        zcashd_path: PathBuf,
        state_cache_dir: &Path,
        network: NetworkKind,
        rpc_url: String,
        cookie_path: PathBuf,
        zebra_rpc_max_response_body_size: usize,
    ) -> Self {
        let extra_args = zcashd_compat.zcashd_extra_args.clone();
        let zcashd_datadir = resolve_zcashd_datadir_path(
            &effective_zcashd_datadir(zcashd_compat, state_cache_dir),
            &extra_args,
        );

        Self {
            zcashd_path,
            zcashd_datadir,
            rpc_url,
            cookie_path,
            enable_cookie_auth: zcashd_compat.enable_cookie_auth,
            tls_ca_file: zcashd_compat.tls_ca_file.clone(),
            zebra_rpc_max_response_body_size,
            extra_args,
            network,
            startup_delay: zcashd_compat.startup_delay,
            restart_backoff: zcashd_compat.restart_backoff,
            restart_backoff_max: zcashd_compat.restart_backoff_max,
            restart_reset_after: zcashd_compat.restart_reset_after,
            shutdown_grace_period: zcashd_compat.shutdown_grace_period,
        }
    }

    /// Builds the zcashd command-line arguments.
    pub fn command_args(&self) -> Vec<String> {
        let mut args = vec![
            "-zebra-compat".to_string(),
            format!("-zebra-compat-url={}", self.rpc_url),
            format!(
                "-zebra-compat-zebra-rpc-max-response-body-bytes={}",
                self.zebra_rpc_max_response_body_size
            ),
            format!("-datadir={}", self.zcashd_datadir.to_string_lossy()),
        ];
        if self.enable_cookie_auth {
            args.push(format!(
                "-zebra-compat-cookiefile={}",
                self.cookie_path.to_string_lossy()
            ));
        } else {
            args.push("-zebra-compat-no-auth=1".to_string());
        }
        if let Some(tls_ca_file) = &self.tls_ca_file {
            args.push(format!(
                "-zebra-compat-tls-ca-file={}",
                tls_ca_file.to_string_lossy()
            ));
        }

        match self.network {
            NetworkKind::Mainnet => {}
            NetworkKind::Testnet => args.push("-testnet".to_string()),
            NetworkKind::Regtest => args.push("-regtest".to_string()),
        }

        // In compat mode zebrad owns P2P. zcashd must not listen on the network
        // port (8233 mainnet / 18233 testnet). Operators often reuse a legacy
        // full-node zcash.conf with listen=1, and CLI args are parsed before
        // zcash.conf without being overwritten by it — so pass these explicitly
        // as defense in depth. zcashd also force-disables P2P boolean flags
        // when -zebra-compat is set, including later extra_args such as -p2p=1.
        // Peer-selection extra_args are still rejected by zcashd startup
        // validation rather than silently taking effect.
        args.push("-p2p=0".to_string());
        args.push("-listen=0".to_string());

        // Always include -printtoconsole and filter it out from extra_args
        args.push("-printtoconsole".to_string());
        args.extend(
            self.extra_args
                .iter()
                .filter(|arg| arg.as_str() != "-printtoconsole")
                .cloned(),
        );
        args
    }
}

/// Runs the zcashd-compat zcashd supervisor until shutdown.
///
/// The supervisor keeps restarting `zcashd` exits that happen before Zebra
/// shutdown, using capped exponential backoff. Spawn failures use the same
/// backoff, so a binary that is briefly missing or unspawnable (for example
/// during an upgrade, or under transient resource pressure) does not
/// permanently end supervision.
///
/// # Errors
///
/// Returns an error if shutdown handling fails.
pub async fn run(
    config: SupervisorConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Report> {
    ensure_zcashd_datadir(&config.zcashd_datadir, &config.extra_args)?;
    set_supervision_active_metrics();

    if wait_for_delay_or_shutdown(config.startup_delay, &mut shutdown_rx).await {
        info!("zcashd-compat supervisor received shutdown during startup delay");
        set_supervision_inactive_metrics();
        return Ok(());
    }

    let mut consecutive_restart_count = 0u32;

    loop {
        if *shutdown_rx.borrow() {
            info!("zcashd-compat supervisor received shutdown before spawn");
            set_supervision_inactive_metrics();
            return Ok(());
        }

        let mut child = match spawn_zcashd(&config) {
            Ok(child) => child,
            Err(error) => {
                consecutive_restart_count = consecutive_restart_count.saturating_add(1);
                warn!(
                    %error,
                    restart_count = consecutive_restart_count,
                    "failed to spawn zcashd-compat zcashd child, retrying after backoff"
                );

                let restart_delay = restart_backoff_delay(
                    config.restart_backoff,
                    config.restart_backoff_max,
                    consecutive_restart_count,
                );
                if wait_for_delay_or_shutdown(restart_delay, &mut shutdown_rx).await {
                    info!("zcashd-compat supervisor received shutdown during spawn retry backoff");
                    set_supervision_inactive_metrics();
                    return Ok(());
                }
                continue;
            }
        };
        let child_started_at = Instant::now();
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
                info!(
                    pid = ?child.id(),
                    grace_period = ?config.shutdown_grace_period,
                    "zcashd-compat supervisor received shutdown request; terminating zcashd child"
                );
                terminate_child(&mut child, config.shutdown_grace_period).await?;
                info!("zcashd-compat zcashd child stopped on shutdown");
                set_supervision_inactive_metrics();
                return Ok(());
            }
            ChildOutcome::Exited(status) => {
                let child_uptime = child_started_at.elapsed();
                if should_reset_restart_count(child_uptime, config.restart_reset_after) {
                    info!(
                        ?status,
                        child_uptime_secs = child_uptime.as_secs(),
                        restart_reset_after_secs = config.restart_reset_after.as_secs(),
                        previous_restart_count = consecutive_restart_count,
                        "zcashd-compat zcashd child had healthy uptime, resetting restart count"
                    );
                    consecutive_restart_count = 0;
                }

                consecutive_restart_count = consecutive_restart_count.saturating_add(1);
                warn!(
                    ?status,
                    restart_count = consecutive_restart_count,
                    child_uptime_secs = child_uptime.as_secs(),
                    "zcashd-compat zcashd child exited before shutdown, restarting"
                );

                let restart_delay = restart_backoff_delay(
                    config.restart_backoff,
                    config.restart_backoff_max,
                    consecutive_restart_count,
                );
                if wait_for_delay_or_shutdown(restart_delay, &mut shutdown_rx).await {
                    info!("zcashd-compat supervisor received shutdown during restart backoff");
                    set_supervision_inactive_metrics();
                    return Ok(());
                }
            }
        }
    }
}

/// Sets metrics for zcashd-compat mode when zcashd supervision is intentionally disabled.
pub fn set_supervision_config_disabled_metrics() {
    metrics::gauge!(SUPERVISOR_ACTIVE_METRIC).set(0.0);
    metrics::gauge!(SUPERVISOR_DISABLED_METRIC).set(1.0);
    metrics::gauge!(SUPERVISOR_EXHAUSTED_METRIC).set(0.0);
}

/// Sets metrics for zcashd-compat mode when supervision has unexpectedly stopped.
pub fn set_supervision_unexpectedly_disabled_metrics() {
    metrics::gauge!(SUPERVISOR_ACTIVE_METRIC).set(0.0);
    metrics::gauge!(SUPERVISOR_DISABLED_METRIC).set(1.0);
}

fn set_supervision_active_metrics() {
    metrics::gauge!(SUPERVISOR_ACTIVE_METRIC).set(1.0);
    metrics::gauge!(SUPERVISOR_DISABLED_METRIC).set(0.0);
    metrics::gauge!(SUPERVISOR_EXHAUSTED_METRIC).set(0.0);
}

fn set_supervision_inactive_metrics() {
    metrics::gauge!(SUPERVISOR_ACTIVE_METRIC).set(0.0);
}

/// Returns `true` when a child ran long enough to make previous failures stale.
fn should_reset_restart_count(child_uptime: Duration, restart_reset_after: Duration) -> bool {
    restart_reset_after != Duration::ZERO && child_uptime >= restart_reset_after
}

/// Calculates capped exponential restart backoff from the base delay and consecutive exit count.
fn restart_backoff_delay(
    base_delay: Duration,
    max_delay: Duration,
    restart_count: u32,
) -> Duration {
    if base_delay == Duration::ZERO || restart_count <= 1 {
        return base_delay.min(max_delay);
    }

    let multiplier = 1u32
        .checked_shl(restart_count.saturating_sub(1))
        .unwrap_or(u32::MAX);
    base_delay.saturating_mul(multiplier).min(max_delay)
}

/// Spawns `zcashd` with zcashd-compat arguments and connects child output streams.
///
/// `kill_on_drop` is intentionally disabled: a dropped child handle (zebrad
/// panic, supervisor task abort) must not SIGKILL a zcashd that may be flushing
/// its chainstate and wallet. An abandoned zcashd finishes any SIGTERM-initiated
/// shutdown on its own, or keeps running until stopped externally; `init` reaps
/// it once zebrad exits. The child also runs in its own process group so
/// group-wide terminal signals aimed at zebrad cannot kill zcashd uncleanly;
/// [`terminate_child`] remains the only path that force-kills it.
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
        .kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);

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
    let pid = child.id();

    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{kill, Signal::SIGTERM},
            unistd::Pid,
        };

        if let Some(id) = pid {
            info!(
                pid = id,
                grace_period = ?shutdown_grace_period,
                "sending SIGTERM to zcashd-compat zcashd child"
            );
            if let Err(error) = kill(Pid::from_raw(id as i32), SIGTERM) {
                warn!(
                    pid = id,
                    ?error,
                    "failed to send SIGTERM to zcashd-compat zcashd child"
                );
            }
        } else {
            warn!("zcashd-compat zcashd child has no process id; cannot send SIGTERM");
        }
    }

    let start = std::time::Instant::now();
    let wait_result = timeout(shutdown_grace_period, child.wait()).await;
    match wait_result {
        Ok(Ok(_status)) => {
            info!(
                ?pid,
                elapsed = ?start.elapsed(),
                "zcashd-compat zcashd exited cleanly after SIGTERM"
            );
            Ok(())
        }
        Ok(Err(error)) => Err(eyre!(
            "failed waiting for zcashd-compat zcashd shutdown: {error}"
        )),
        Err(_timeout) => {
            warn!(
                ?pid,
                grace_period = ?shutdown_grace_period,
                "zcashd-compat zcashd did not exit after SIGTERM, sending kill; \
                 an interrupted shutdown can lose un-flushed chainstate"
            );
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

    use super::{
        restart_backoff_delay, should_reset_restart_count, wait_for_delay_or_shutdown,
        SupervisorConfig,
    };

    #[test]
    fn command_args_include_zcashd_compat_flags() {
        let config = SupervisorConfig {
            zcashd_path: PathBuf::from("zcashd"),
            zcashd_datadir: PathBuf::from("/tmp/zcashd-compat-datadir"),
            rpc_url: "http://127.0.0.1:8232".to_string(),
            cookie_path: PathBuf::from("/tmp/.cookie"),
            enable_cookie_auth: true,
            tls_ca_file: None,
            zebra_rpc_max_response_body_size: 128 * 1024 * 1024,
            extra_args: vec!["-debug=1".to_string()],
            network: NetworkKind::Regtest,
            startup_delay: Duration::from_secs(1),
            restart_backoff: Duration::from_secs(2),
            restart_backoff_max: Duration::from_secs(5 * 60),
            restart_reset_after: Duration::from_secs(60 * 60),
            shutdown_grace_period: Duration::from_secs(300),
        };

        let args = config.command_args();

        assert!(args.contains(&"-zebra-compat".to_string()));
        assert!(args.contains(&"-regtest".to_string()));
        assert!(args
            .iter()
            .any(|a| a.starts_with("-zebra-compat-url=http://127.0.0.1:8232")));
        assert!(args
            .iter()
            .any(|a| a.starts_with("-zebra-compat-cookiefile=/tmp/.cookie")));
        assert!(args
            .iter()
            .any(|a| a == "-zebra-compat-zebra-rpc-max-response-body-bytes=134217728"));
        assert!(args.contains(&"-p2p=0".to_string()));
        assert!(args.contains(&"-listen=0".to_string()));
        assert!(args.contains(&"-printtoconsole".to_string()));
        assert!(args.contains(&"-debug=1".to_string()));

        let p2p_idx = args
            .iter()
            .position(|a| a == "-p2p=0")
            .expect("p2p override present");
        let listen_idx = args
            .iter()
            .position(|a| a == "-listen=0")
            .expect("listen override present");
        let debug_idx = args
            .iter()
            .position(|a| a == "-debug=1")
            .expect("extra arg present");
        assert!(p2p_idx < debug_idx);
        assert!(listen_idx < debug_idx);
    }

    #[test]
    fn command_args_use_explicit_no_auth_flag_when_cookie_auth_disabled() {
        let config = SupervisorConfig {
            zcashd_path: PathBuf::from("zcashd"),
            zcashd_datadir: PathBuf::from("/tmp/zcashd-compat-datadir"),
            rpc_url: "https://127.0.0.1:8232".to_string(),
            cookie_path: PathBuf::from("/tmp/.cookie"),
            enable_cookie_auth: false,
            tls_ca_file: Some(PathBuf::from("/tmp/ca.pem")),
            zebra_rpc_max_response_body_size: 128 * 1024 * 1024,
            extra_args: Vec::new(),
            network: NetworkKind::Regtest,
            startup_delay: Duration::from_secs(1),
            restart_backoff: Duration::from_secs(2),
            restart_backoff_max: Duration::from_secs(5 * 60),
            restart_reset_after: Duration::from_secs(60 * 60),
            shutdown_grace_period: Duration::from_secs(300),
        };

        let args = config.command_args();

        assert!(args.contains(&"-zebra-compat-no-auth=1".to_string()));
        assert!(args.contains(&"-zebra-compat-tls-ca-file=/tmp/ca.pem".to_string()));
        assert!(!args
            .iter()
            .any(|arg| arg.starts_with("-zebra-compat-cookiefile=")));
    }

    #[test]
    fn restart_count_resets_after_healthy_uptime() {
        assert!(should_reset_restart_count(
            Duration::from_secs(60 * 60),
            Duration::from_secs(60 * 60)
        ));
        assert!(should_reset_restart_count(
            Duration::from_secs(60 * 60 + 1),
            Duration::from_secs(60 * 60)
        ));
    }

    #[test]
    fn restart_count_does_not_reset_before_threshold() {
        assert!(!should_reset_restart_count(
            Duration::from_secs(60 * 60 - 1),
            Duration::from_secs(60 * 60)
        ));
        assert!(!should_reset_restart_count(
            Duration::from_secs(60 * 60),
            Duration::ZERO
        ));
    }

    #[test]
    fn restart_backoff_is_exponential_from_base_delay() {
        let base_delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(60);

        assert_eq!(restart_backoff_delay(base_delay, max_delay, 0), base_delay);
        assert_eq!(restart_backoff_delay(base_delay, max_delay, 1), base_delay);
        assert_eq!(
            restart_backoff_delay(base_delay, max_delay, 2),
            Duration::from_secs(4)
        );
        assert_eq!(
            restart_backoff_delay(base_delay, max_delay, 3),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn restart_backoff_is_capped() {
        let delay = restart_backoff_delay(Duration::from_secs(2), Duration::from_secs(10), 10);

        assert_eq!(delay, Duration::from_secs(10));
    }

    #[test]
    fn restart_backoff_caps_saturated_delay() {
        let delay = restart_backoff_delay(Duration::MAX, Duration::from_secs(10), u32::MAX);

        assert_eq!(delay, Duration::from_secs(10));
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

    /// A child that exits on SIGTERM within the grace period is never SIGKILLed,
    /// so its shutdown flush cannot be interrupted.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_waits_for_graceful_exit() {
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(false)
            .spawn()
            .expect("sleep is available on unix test hosts");

        let start = std::time::Instant::now();
        super::terminate_child(&mut child, Duration::from_secs(30))
            .await
            .expect("terminate_child should succeed for a SIGTERM-compliant child");

        assert!(
            start.elapsed() < Duration::from_secs(30),
            "child should exit on SIGTERM well before the grace period"
        );
    }

    /// A child that ignores SIGTERM is force-killed only after the full grace
    /// period elapses.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn terminate_child_kills_after_grace_period() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; sleep 60"])
            .kill_on_drop(true)
            .spawn()
            .expect("sh is available on unix test hosts");

        // Give the shell a moment of real time to install the TERM trap before
        // SIGTERM is sent; the paused clock only skips tokio timers.
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(200));

        super::terminate_child(&mut child, Duration::from_secs(5))
            .await
            .expect("terminate_child should fall back to SIGKILL");

        let status = child
            .try_wait()
            .expect("child status should be queryable after terminate_child");
        assert!(
            status.is_some(),
            "child must have been reaped after the SIGKILL fallback"
        );
    }
}
