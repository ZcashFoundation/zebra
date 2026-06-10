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
    config::{build_zcashd_compat_config, ZCASHD_TEST_RPC_PASS, ZCASHD_TEST_RPC_USER},
    ZcashdRpcClient, TEST_ZCASHD_COOKIE_FILE, TEST_ZCASHD_RPC_ADDR, TEST_ZCASHD_RPC_PASSWORD,
    TEST_ZCASHD_RPC_USER, TEST_ZEBRAD_RPC_ADDR,
};
use crate::common::{
    config::{read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

/// Zcashd-compat cookie file name written by zebrad.
const ZEBRA_COMPAT_COOKIE_FILE_NAME: &str = ".zcashd-compat.cookie";

/// How long to poll zcashd's own RPC before giving up.
const ZCASHD_RPC_POLL_ATTEMPTS: u32 = 60;
const ZCASHD_RPC_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The live context for a zcashd-compat integration test.
pub struct ZcashdCompatSetup {
    /// The zebrad process, present only in managed (regtest) mode.
    pub managed: Option<TestChild<TempDir>>,
    /// Client for zebrad's unauthenticated main RPC.
    pub zebra_client: RpcRequestClient,
    /// Client for zcashd's own authenticated RPC (wallet operations).
    pub zcashd_client: ZcashdRpcClient,
    /// The network under test.
    pub network: Network,
    /// Zebrad main RPC address.
    #[allow(dead_code)]
    pub zebra_rpc_addr: SocketAddr,
    /// Zebrad zcashd-compat RPC address (cookie-authenticated; used in auth tests).
    pub zebra_compat_rpc_addr: SocketAddr,
}

impl ZcashdCompatSetup {
    /// Returns `true` iff this is a managed regtest session where mining and
    /// wallet mutations are safe.
    pub fn can_mutate(&self) -> bool {
        self.network.is_regtest()
    }

    /// Cleans up: kills the managed zebrad child if present, asserts clean exit.
    pub fn teardown(mut self) -> Result<()> {
        if let Some(mut z) = self.managed.take() {
            z.kill(false)?;
            z.wait_with_output()?
                .assert_failure()?
                .assert_was_killed()?;
        }
        Ok(())
    }
}

// ── Managed (regtest) mode ────────────────────────────────────────────────────

/// Spawns a fresh regtest zebrad that supervises a zcashd-compat zcashd process,
/// waits for both to be ready, and returns the test setup.
pub async fn spawn_zebrad_with_zcashd_compat() -> Result<ZcashdCompatSetup> {
    let _init_guard = zebra_test::init();

    let dir = testdir()?;
    let cookie_dir = dir.path().to_path_buf();

    let compat_cfg = build_zcashd_compat_config(cookie_dir.clone())?;
    let mut zebrad_config = compat_cfg.zebrad_config;

    let zebra_rpc_addr = compat_cfg.zebra_rpc_addr;
    let zebra_compat_rpc_addr = compat_cfg.zebra_compat_rpc_addr;
    let zcashd_own_rpc_addr = compat_cfg.zcashd_own_rpc_addr;

    // `--unsafe-low-specs` skips the hardware preflight minimums (600 GiB disk
    // etc.), which regtest doesn't need and CI runners don't have.
    let mut zebrad = dir.with_config(&mut zebrad_config)?.spawn_child(args![
        "start",
        "--zcashd-compat",
        "--unsafe-low-specs"
    ])?;

    // Main RPC logs first; zcashd-compat RPC logs second.
    // We pre-chose both ports via random_known_port(), so we only need to wait
    // for the log lines to confirm zebrad is ready — we don't parse the addresses.
    let _ = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;
    let _ = read_listen_addr_from_logs(&mut zebrad, OPENED_RPC_ENDPOINT_MSG)?;

    // Extra stability margin before poking zcashd
    sleep(LAUNCH_DELAY).await;

    // Zcashd-compat cookie is written by the zcashd-compat RPC server at startup
    let cookie_path = cookie_dir.join(ZEBRA_COMPAT_COOKIE_FILE_NAME);
    wait_for_cookie_file(&cookie_path).await?;

    let zebra_client = RpcRequestClient::new(zebra_rpc_addr);
    let zcashd_client = ZcashdRpcClient::new(
        zcashd_own_rpc_addr,
        ZCASHD_TEST_RPC_USER,
        ZCASHD_TEST_RPC_PASS,
    );

    wait_for_zcashd_rpc(&zcashd_client).await?;

    Ok(ZcashdCompatSetup {
        managed: Some(zebrad),
        zebra_client,
        zcashd_client,
        network: Network::new_regtest(Default::default()),
        zebra_rpc_addr,
        zebra_compat_rpc_addr,
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

    // In external mode there is no zcashd-compat RPC addr in our control;
    // use the zebrad main RPC addr as a stand-in (auth tests skip on external).
    let zebra_compat_rpc_addr = zebra_rpc_addr;

    Ok(ZcashdCompatSetup {
        managed: None,
        zebra_client: RpcRequestClient::new(zebra_rpc_addr),
        zcashd_client,
        network,
        zebra_rpc_addr,
        zebra_compat_rpc_addr,
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

/// Waits for the zcashd-compat cookie file to appear on disk (up to 30 s).
async fn wait_for_cookie_file(path: &std::path::Path) -> Result<()> {
    for attempt in 1..=30u32 {
        if path.exists() {
            return Ok(());
        }
        if attempt == 30 {
            return Err(eyre!(
                "zcashd-compat cookie file never appeared at {}",
                path.display()
            ));
        }
        sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}
