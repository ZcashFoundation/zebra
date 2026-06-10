//! Watchdog configuration, sourced from CLI flags and environment variables.
//!
//! Environment variable names intentionally match the ones used by
//! `deploy/zcashd-compat/sync-check.sh` and the `deploy-zcashd-compat.yml`
//! workflow, so the watchdog is a drop-in replacement for the shell check.

use std::path::PathBuf;

use clap::Args;

/// Configuration shared by all watchdog checks and run modes.
#[derive(Args, Clone, Debug)]
pub struct Config {
    /// Zebra JSON-RPC endpoint URL.
    #[arg(
        long,
        global = true,
        env = "ZEBRA_RPC_URL",
        default_value = "http://127.0.0.1:8232"
    )]
    pub zebra_rpc_url: String,

    /// Path to the Zebra RPC cookie file.
    #[arg(
        global = true,
        long,
        env = "ZEBRA_COOKIE_FILE",
        default_value = "/root/.cache/zebra/.cookie"
    )]
    pub zebra_cookie_file: PathBuf,

    /// zcashd JSON-RPC endpoint URL.
    #[arg(
        long,
        global = true,
        env = "ZCASHD_RPC_URL",
        default_value = "http://[::1]:8232"
    )]
    pub zcashd_rpc_url: String,

    /// Path to the zcashd RPC cookie file.
    #[arg(
        global = true,
        long,
        env = "ZCASHD_COOKIE_FILE",
        default_value = "/mnt/snapshots/runtime/zcashd/.cookie"
    )]
    pub zcashd_cookie_file: PathBuf,

    /// `pgrep -f` pattern that must match a running zebrad process.
    #[arg(
        global = true,
        long,
        env = "ZEBRAD_PROCESS_PATTERN",
        default_value = "zebrad .*--zcashd-compat"
    )]
    pub zebrad_process_pattern: String,

    /// `pgrep -f` pattern that must match a running zcashd process.
    #[arg(
        global = true,
        long,
        env = "ZCASHD_PROCESS_PATTERN",
        default_value = "zcashd .*-zebra-compat"
    )]
    pub zcashd_process_pattern: String,

    /// Maximum allowed absolute height drift between zebrad and zcashd.
    #[arg(long, global = true, env = "HEIGHT_MAX_DRIFT", default_value_t = 10)]
    pub height_max_drift: u64,

    /// Seconds to keep retrying before a one-shot `check` fails.
    #[arg(long, global = true, env = "SYNC_CHECK_TIMEOUT", default_value_t = 600)]
    pub sync_check_timeout: u64,

    /// Seconds between retries during a one-shot `check`.
    #[arg(long, global = true, env = "SYNC_CHECK_INTERVAL", default_value_t = 15)]
    pub sync_check_interval: u64,

    /// Seconds between check cycles in continuous `run` mode.
    #[arg(long, global = true, env = "WATCHDOG_INTERVAL", default_value_t = 60)]
    pub watchdog_interval: u64,

    /// Per-request RPC timeout in seconds.
    #[arg(
        long,
        global = true,
        env = "WATCHDOG_RPC_TIMEOUT",
        default_value_t = 30
    )]
    pub rpc_timeout: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;

    /// Test harness mirroring the real CLI shape, so `Config` flattening
    /// behaves exactly as in `main`.
    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        config: Config,
    }

    #[test]
    fn defaults_match_sync_check_script() {
        let cli = TestCli::try_parse_from(["zebra-watchdog"]).expect("defaults parse");
        let config = cli.config;

        assert_eq!(config.zebra_rpc_url, "http://127.0.0.1:8232");
        assert_eq!(config.zcashd_rpc_url, "http://[::1]:8232");
        assert_eq!(config.zebrad_process_pattern, "zebrad .*--zcashd-compat");
        assert_eq!(config.zcashd_process_pattern, "zcashd .*-zebra-compat");
        assert_eq!(config.height_max_drift, 10);
        assert_eq!(config.sync_check_timeout, 600);
        assert_eq!(config.sync_check_interval, 15);
        assert_eq!(config.watchdog_interval, 60);
    }

    #[test]
    fn cli_flags_override_defaults() {
        let cli = TestCli::try_parse_from([
            "zebra-watchdog",
            "--zebra-rpc-url",
            "http://127.0.0.1:18232",
            "--height-max-drift",
            "3",
            "--watchdog-interval",
            "5",
        ])
        .expect("flags parse");
        let config = cli.config;

        assert_eq!(config.zebra_rpc_url, "http://127.0.0.1:18232");
        assert_eq!(config.height_max_drift, 3);
        assert_eq!(config.watchdog_interval, 5);
    }
}
