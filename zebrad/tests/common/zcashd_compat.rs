//! Shared infrastructure for the zcashd-compat integration test suite.
//!
//! # Warning
//!
//! Test functions in this file and its submodules will not be run.
//! This file is only for test library code.

use std::{net::SocketAddr, path::Path};

use color_eyre::eyre::{eyre, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use zebra_node_services::BoxError;

pub mod chain;
pub mod config;
pub mod launch;
pub mod network;
pub mod reorg;
pub mod resilience;
pub mod startup;
pub mod tx_flow;
pub mod wallet;

// ── Environment variable constants ───────────────────────────────────────────

/// Enable zcashd-compat integration tests.
/// Set to any non-empty value (e.g. `1`) to run the suite.
pub const TEST_ZCASHD_COMPAT: &str = "TEST_ZCASHD_COMPAT";

// NOTE: these test env vars deliberately avoid the `ZEBRA_` prefix, because
// zebrad parses every inherited `ZEBRA_*` env var as a config override, and
// `ZebradConfig` rejects unknown fields. A `ZEBRA_`-prefixed test var would
// make every spawned zebrad child abort at startup.

/// Optional explicit path to a P2P-sidecar zcashd binary.
/// If unset, the embedded download is used in regtest mode.
pub const TEST_ZCASHD_PATH: &str = "TEST_ZCASHD_PATH";

/// Network for external mode: `Mainnet` or `Testnet`.
/// Absent means regtest/managed.
pub const TEST_ZCASHD_COMPAT_NETWORK: &str = "TEST_ZCASHD_COMPAT_NETWORK";

/// Zebrad main RPC address for external mode (e.g. `127.0.0.1:8232`).
pub const TEST_ZEBRAD_RPC_ADDR: &str = "TEST_ZEBRAD_RPC_ADDR";

/// Zcashd own RPC address for external mode (e.g. `127.0.0.1:8233`).
pub const TEST_ZCASHD_RPC_ADDR: &str = "TEST_ZCASHD_RPC_ADDR";

/// Zcashd RPC username (user/pass auth, alternative to cookie file).
pub const TEST_ZCASHD_RPC_USER: &str = "TEST_ZCASHD_RPC_USER";

/// Zcashd RPC password (user/pass auth, alternative to cookie file).
pub const TEST_ZCASHD_RPC_PASSWORD: &str = "TEST_ZCASHD_RPC_PASSWORD";

/// Path to zcashd cookie file for external mode (preferred over user/pass).
pub const TEST_ZCASHD_COOKIE_FILE: &str = "TEST_ZCASHD_COOKIE_FILE";

/// Number of reorg churn cycles to run in the zcashd-compat stress test.
pub const TEST_ZCASHD_COMPAT_REORG_ITERATIONS: &str = "TEST_ZCASHD_COMPAT_REORG_ITERATIONS";

/// Enable slow zcashd restart-after-reorg integration probes.
pub const TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG: &str = "TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG";

// ── Skip guard ────────────────────────────────────────────────────────────────

/// Returns `true` and prints a message if zcashd-compat tests are disabled.
#[allow(clippy::print_stderr)]
pub fn zebra_skip_zcashd_compat_tests() -> bool {
    if std::env::var_os(TEST_ZCASHD_COMPAT).is_none() {
        eprintln!(
            "Skipped zcashd-compat integration test; \
             set {TEST_ZCASHD_COMPAT}=1 to run"
        );
        return true;
    }
    false
}

// ── RPC client with HTTP Basic Auth ──────────────────────────────────────────

/// An HTTP JSON-RPC client that authenticates every request with HTTP Basic Auth.
///
/// Used to talk to zcashd's own RPC endpoint (which requires credentials).
#[derive(Clone, Debug)]
pub struct ZcashdRpcClient {
    client: Client,
    addr: SocketAddr,
    user: String,
    pass: String,
}

impl ZcashdRpcClient {
    /// Creates a new client using explicit username and password credentials.
    pub fn new(addr: SocketAddr, user: impl Into<String>, pass: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            addr,
            user: user.into(),
            pass: pass.into(),
        }
    }

    /// Creates a new client by reading credentials from a `__cookie__:PASSWORD` cookie file.
    pub fn from_cookie_file(addr: SocketAddr, cookie_path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(cookie_path)
            .map_err(|e| eyre!("failed to read cookie file {}: {e}", cookie_path.display()))?;
        let trimmed = contents.trim();
        let colon = trimmed
            .find(':')
            .ok_or_else(|| eyre!("invalid cookie file: no ':' separator"))?;
        Ok(Self::new(addr, &trimmed[..colon], &trimmed[colon + 1..]))
    }

    /// Sends a JSON-RPC call authenticated with Basic Auth.
    pub async fn call(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> reqwest::Result<reqwest::Response> {
        let method = method.as_ref();
        let params = params.as_ref();
        self.client
            .post(format!("http://{}", self.addr))
            .basic_auth(&self.user, Some(&self.pass))
            .header("Content-Type", "application/json")
            .body(format!(
                r#"{{"jsonrpc":"2.0","method":"{method}","params":{params},"id":123}}"#
            ))
            .send()
            .await
    }

    /// Returns the RPC address this client is configured to connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Sends a call and attempts to deserialize the `result` field.
    ///
    /// zcashd returns JSON-RPC 1.0-style responses where both `result` and
    /// `error` are always present and the unused one is `null`, so this parses
    /// the fields manually instead of using `jsonrpsee_types::Response`
    /// (which rejects `"error": null`).
    pub async fn json_result_from_call<T: DeserializeOwned>(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> std::result::Result<T, BoxError> {
        let text = self.call(method, params).await?.text().await?;
        let mut response: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text)?;
        match response.remove("error") {
            Some(error) if !error.is_null() => Err(error.to_string().into()),
            _ => {
                let result = response.remove("result").unwrap_or(serde_json::Value::Null);
                Ok(serde_json::from_value(result)?)
            }
        }
    }
}

/// Polls zcashd's `getblockcount` until it reaches `height`, up to 60 seconds.
///
/// zcashd learns about newly mined blocks from zebrad over legacy P2P block
/// gossip, so tests that mine must wait for it to catch up before cross-checking.
pub async fn wait_for_zcashd_height(client: &ZcashdRpcClient, height: u64) -> Result<()> {
    let mut last_seen = None;
    for _ in 0..60u32 {
        if let Ok(count) = client
            .json_result_from_call::<u64>("getblockcount", "[]")
            .await
        {
            if count >= height {
                return Ok(());
            }
            last_seen = Some(count);
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(eyre!(
        "zcashd did not reach height {height} within 60 s (last seen: {last_seen:?})"
    ))
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Sets up the zcashd-compat test environment, dispatching between managed
/// (regtest) and external (mainnet/testnet) mode based on env vars.
///
/// Returns `None` if zcashd-compat tests are disabled (`TEST_ZCASHD_COMPAT` unset).
pub async fn setup_zcashd_compat() -> Result<Option<launch::ZcashdCompatSetup>> {
    if zebra_skip_zcashd_compat_tests() {
        return Ok(None);
    }

    use zebra_chain::parameters::NetworkKind;

    match config::read_test_network_kind()? {
        NetworkKind::Regtest => launch::spawn_zebrad_with_zcashd_compat().await.map(Some),
        kind => launch::connect_to_external_zcashd_compat(kind)
            .await
            .map(Some),
    }
}
