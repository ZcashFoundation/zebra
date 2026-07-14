//! Process spawning and connection helpers for the zcashd-compat test suite.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tempfile::TempDir;
use tokio::time::sleep;
use zebra_chain::parameters::{Network, NetworkKind};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
use zebra_test::{args, command::TestChild};

use super::{
    config::{
        build_zcashd_compat_config, ZcashdCompatConfig, ZCASHD_TEST_RPC_PASS, ZCASHD_TEST_RPC_USER,
    },
    ZcashdRpcClient, TEST_ZCASHD_COOKIE_FILE, TEST_ZCASHD_RPC_ADDR, TEST_ZCASHD_RPC_PASSWORD,
    TEST_ZCASHD_RPC_USER, TEST_ZEBRAD_RPC_ADDR,
};
use crate::common::{
    config::{read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

/// How long to poll zcashd's own RPC before giving up.
const ZCASHD_RPC_POLL_ATTEMPTS: u32 = 60;
const ZCASHD_RPC_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The live context for a zcashd-compat integration test.
pub struct ZcashdCompatSetup {
    /// The zebrad process, present only in managed (regtest) mode.
    pub managed: Option<TestChild<TempDir>>,
    /// Zcashd datadir, present only when the test harness manages zcashd.
    pub zcashd_datadir: Option<PathBuf>,
    /// Client for zebrad's unauthenticated main RPC.
    pub zebra_client: RpcRequestClient,
    /// Client for zcashd's own authenticated RPC (wallet operations).
    pub zcashd_client: ZcashdRpcClient,
    /// The network under test.
    pub network: Network,
    /// Zebrad main RPC address.
    #[allow(dead_code)]
    pub zebra_rpc_addr: SocketAddr,
}

impl ZcashdCompatSetup {
    /// Returns `true` iff this is a managed regtest session where mining and
    /// wallet mutations are safe.
    pub fn can_mutate(&self) -> bool {
        self.network.is_regtest()
    }

    /// Returns the pid of the supervised zcashd from its pid file.
    ///
    /// Errors in external mode (no managed datadir) or before zcashd has
    /// written its pid file.
    pub fn zcashd_pid(&self) -> Result<u32> {
        let datadir = self
            .zcashd_datadir
            .as_ref()
            .ok_or_else(|| eyre!("zcashd datadir is unavailable outside managed regtest mode"))?;
        let pid_path = datadir.join("regtest").join("zcashd.pid");
        let pid = std::fs::read_to_string(&pid_path)
            .map_err(|e| eyre!("failed to read zcashd pid file {}: {e}", pid_path.display()))?;

        pid.trim()
            .parse()
            .map_err(|e| eyre!("invalid zcashd pid in {}: {e}", pid_path.display()))
    }

    /// Cleans up: kills the managed zebrad child if present (asserting it was
    /// killed cleanly), then kills the supervised zcashd it leaves behind.
    ///
    /// SIGKILLing zebrad skips the supervisor's zcashd shutdown path, so
    /// without the explicit kill every managed test leaks one zcashd process.
    pub fn teardown(mut self) -> Result<()> {
        // Read the pid before killing zebrad: the testdir holding the pid file
        // is dropped with the zebrad `TestChild`.
        let zcashd_pid = self.zcashd_pid().ok();

        if let Some(mut z) = self.managed.take() {
            z.kill(false)?;
            z.wait_with_output()?
                .assert_failure()?
                .assert_was_killed()?;
        }

        // Best-effort: zcashd may already have exited (resilience tests stop it).
        if let Some(pid) = zcashd_pid {
            let _ = send_signal(pid, "-KILL");
        }
        Ok(())
    }
}

impl Drop for ZcashdCompatSetup {
    fn drop(&mut self) {
        // Failure paths return early without calling `teardown()`; zebrad is
        // killed by the `TestChild` drop, but the supervised zcashd would leak.
        // After a successful `teardown()` the testdir is already gone, so the
        // pid read fails and this is a no-op.
        if let Ok(pid) = self.zcashd_pid() {
            let _ = send_signal(pid, "-KILL");
        }
    }
}

/// Sends `signal` (a `kill` argument like `-STOP` or `-KILL`) to `pid`.
pub fn send_signal(pid: u32, signal: &str) -> Result<()> {
    let status = std::process::Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|e| eyre!("failed to run kill {signal} {pid}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(eyre!("kill {signal} {pid} failed with status {status}"))
    }
}

// ── Managed (regtest) mode ────────────────────────────────────────────────────

/// Spawns a fresh regtest zebrad that supervises a zcashd-compat zcashd process,
/// waits for both to be ready, and returns the test setup.
pub async fn spawn_zebrad_with_zcashd_compat() -> Result<ZcashdCompatSetup> {
    let _init_guard = zebra_test::init();

    let dir = testdir()?;
    let work_dir = dir.path().to_path_buf();

    let compat_cfg: ZcashdCompatConfig = build_zcashd_compat_config(work_dir)?;
    let mut zebrad_config = compat_cfg.zebrad_config;

    let zebra_rpc_addr = compat_cfg.zebra_rpc_addr;
    let zcashd_own_rpc_addr = compat_cfg.zcashd_own_rpc_addr;

    // `--unsafe-low-specs` skips the hardware preflight minimums (550 GiB disk
    // etc.), which regtest doesn't need and CI runners don't have.
    let mut zebrad = dir.with_config(&mut zebrad_config)?.spawn_child(args![
        "start",
        "--zcashd-compat",
        "--unsafe-low-specs"
    ])?;

    let _ = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;

    // Extra stability margin before poking zcashd
    sleep(LAUNCH_DELAY).await;

    let zebra_client = RpcRequestClient::new(zebra_rpc_addr);
    let zcashd_client = ZcashdRpcClient::new(
        zcashd_own_rpc_addr,
        ZCASHD_TEST_RPC_USER,
        ZCASHD_TEST_RPC_PASS,
    );

    wait_for_zcashd_rpc(&zcashd_client).await?;

    Ok(ZcashdCompatSetup {
        managed: Some(zebrad),
        zcashd_datadir: Some(compat_cfg.zcashd_datadir),
        zebra_client,
        zcashd_client,
        network: Network::new_regtest(Default::default()),
        zebra_rpc_addr,
    })
}

// ── External (mainnet / testnet) mode ─────────────────────────────────────────

/// Connects to pre-running zebrad and zcashd instances for mainnet or testnet
/// validation. Returns `Err` if required environment variables are missing.
pub async fn connect_to_external_zcashd_compat(kind: NetworkKind) -> Result<ZcashdCompatSetup> {
    let _init_guard = zebra_test::init();

    let zebra_rpc_addr: SocketAddr = std::env::var(TEST_ZEBRAD_RPC_ADDR)
        .map_err(|_| eyre!("{TEST_ZEBRAD_RPC_ADDR} must be set for external mode"))?
        .parse()
        .map_err(|e| eyre!("invalid {TEST_ZEBRAD_RPC_ADDR}: {e}"))?;

    let zcashd_own_rpc_addr: SocketAddr = std::env::var(TEST_ZCASHD_RPC_ADDR)
        .map_err(|_| eyre!("{TEST_ZCASHD_RPC_ADDR} must be set for external mode"))?
        .parse()
        .map_err(|e| eyre!("invalid {TEST_ZCASHD_RPC_ADDR}: {e}"))?;

    let zcashd_client = if let Ok(cookie_path) = std::env::var(TEST_ZCASHD_COOKIE_FILE) {
        ZcashdRpcClient::from_cookie_file(zcashd_own_rpc_addr, &PathBuf::from(cookie_path))?
    } else {
        let user = std::env::var(TEST_ZCASHD_RPC_USER).map_err(|_| {
            eyre!(
                "either {TEST_ZCASHD_COOKIE_FILE} or \
                 {TEST_ZCASHD_RPC_USER}/{TEST_ZCASHD_RPC_PASSWORD} must be set"
            )
        })?;
        let pass = std::env::var(TEST_ZCASHD_RPC_PASSWORD).unwrap_or_default();
        ZcashdRpcClient::new(zcashd_own_rpc_addr, user, pass)
    };

    wait_for_zcashd_rpc(&zcashd_client).await?;

    let network = match kind {
        NetworkKind::Mainnet => Network::Mainnet,
        NetworkKind::Testnet => Network::new_default_testnet(),
        NetworkKind::Regtest => {
            unreachable!("regtest is handled by spawn_zebrad_with_zcashd_compat")
        }
    };

    Ok(ZcashdCompatSetup {
        managed: None,
        zcashd_datadir: None,
        zebra_client: RpcRequestClient::new(zebra_rpc_addr),
        zcashd_client,
        network,
        zebra_rpc_addr,
    })
}

// ── Polling helpers ────────────────────────────────────────────────────────────

/// Polls zcashd's own RPC until `getblockchaininfo` succeeds, up to a 2-minute
/// limit.  Returns `Err` after exhausting all attempts.
pub async fn wait_for_zcashd_rpc(client: &ZcashdRpcClient) -> Result<()> {
    for attempt in 1..=ZCASHD_RPC_POLL_ATTEMPTS {
        let result = client
            .json_result_from_call::<serde_json::Value>("getblockchaininfo", "[]")
            .await;
        if result.is_ok() {
            return Ok(());
        }
        if attempt == ZCASHD_RPC_POLL_ATTEMPTS {
            return Err(eyre!(
                "zcashd RPC at {} did not respond after {} attempts: {}",
                client.addr(),
                ZCASHD_RPC_POLL_ATTEMPTS,
                result.unwrap_err(),
            ));
        }
        sleep(ZCASHD_RPC_POLL_INTERVAL).await;
    }
    Ok(())
}
