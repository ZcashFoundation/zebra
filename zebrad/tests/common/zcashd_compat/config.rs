//! Config building for the zcashd-compat integration test suite.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use color_eyre::eyre::Result;
use zebra_chain::parameters::{
    testnet::ConfiguredActivationHeights, Network, NetworkKind,
};
use zebra_rpc::config::mining::MinerAddressType;
use zebra_test::net::random_known_port;
use zebrad::{
    components::{mempool, With},
    config::ZebradConfig,
};

use crate::common::config::default_test_config;
use super::ZEBRA_TEST_ZCASHD_PATH;

/// Configuration produced by [`build_zcashd_compat_config`].
pub struct ZcashdCompatConfig {
    pub zebrad_config: ZebradConfig,
    /// Zebrad's main (unauthenticated) RPC listen address.
    pub zebra_rpc_addr: SocketAddr,
    /// Zebrad's zcashd-compat (cookie-authenticated) RPC listen address.
    pub zebra_compat_rpc_addr: SocketAddr,
    /// Zcashd's own RPC listen address (user/pass authenticated).
    pub zcashd_own_rpc_addr: SocketAddr,
}

/// Hardcoded test credentials injected into zcashd via `-rpcuser`/`-rpcpassword`.
pub const ZCASHD_TEST_RPC_USER: &str = "zcashd_test";
pub const ZCASHD_TEST_RPC_PASS: &str = "zebra_test_pass";

/// Builds a regtest zebrad config wired for zcashd-compat testing.
///
/// `cookie_dir` must be the directory where zebrad will write the zcashd-compat
/// cookie file.  In managed-spawn mode this is the testdir (kept alive by the
/// `TestChild`).
pub fn build_zcashd_compat_config(cookie_dir: PathBuf) -> Result<ZcashdCompatConfig> {
    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );

    let zebra_rpc_port = random_known_port();
    let zcashd_compat_port = random_known_port();
    let zcashd_own_rpc_port = random_known_port();

    let zebra_rpc_addr: SocketAddr = format!("127.0.0.1:{zebra_rpc_port}").parse()?;
    let zebra_compat_rpc_addr: SocketAddr = format!("127.0.0.1:{zcashd_compat_port}").parse()?;
    let zcashd_own_rpc_addr: SocketAddr = format!("127.0.0.1:{zcashd_own_rpc_port}").parse()?;

    let mut config = default_test_config(&net)
        .with(MinerAddressType::Transparent);

    // Main RPC: no cookie auth, single-threaded for test determinism
    config.rpc.listen_addr = Some(zebra_rpc_addr);
    config.rpc.parallel_cpu_threads = 1;
    config.rpc.enable_cookie_auth = false;

    // Enable mempool from genesis so tx-flow tests work immediately
    config.mempool = mempool::Config {
        debug_enable_at_height: Some(0),
        ..config.mempool
    };

    // Zcashd-compat mode
    config.zcashd_compat.enabled = true;
    config.zcashd_compat.manage_zcashd = true;
    config.zcashd_compat.listen_addr = Some(zebra_compat_rpc_addr);
    config.zcashd_compat.cookie_dir = cookie_dir;
    // Skip startup delay in tests — supervisor spawns zcashd immediately
    config.zcashd_compat.startup_delay = Duration::ZERO;

    // Use an explicit zcashd path if provided, else managed download
    if let Some(path) = std::env::var_os(ZEBRA_TEST_ZCASHD_PATH) {
        config.zcashd_compat.zcashd_source =
            zebrad::components::zcashd_compat::ConfigZcashdBinarySource::Path;
        config.zcashd_compat.zcashd_path = Some(PathBuf::from(path));
    }

    // Expose zcashd's own RPC on a known port with simple test credentials
    config.zcashd_compat.zcashd_extra_args = vec![
        format!("-rpcport={zcashd_own_rpc_port}"),
        format!("-rpcuser={ZCASHD_TEST_RPC_USER}"),
        format!("-rpcpassword={ZCASHD_TEST_RPC_PASS}"),
        "-rpcallowip=127.0.0.1".to_string(),
    ];

    Ok(ZcashdCompatConfig {
        zebrad_config: config,
        zebra_rpc_addr,
        zebra_compat_rpc_addr,
        zcashd_own_rpc_addr,
    })
}

/// Returns the expected zcashd `chain` field value for the given network.
pub fn expected_chain_name(network: &Network) -> &'static str {
    match network.kind() {
        NetworkKind::Mainnet => "main",
        NetworkKind::Testnet => "test",
        NetworkKind::Regtest => "regtest",
    }
}

/// Reads `ZEBRA_TEST_ZCASHD_COMPAT_NETWORK` and returns the corresponding
/// [`NetworkKind`].  Defaults to `Regtest` when absent.
///
/// Returns `Err` for unrecognised values.
pub fn read_test_network_kind() -> Result<NetworkKind> {
    match std::env::var(super::ZEBRA_TEST_ZCASHD_COMPAT_NETWORK)
        .ok()
        .as_deref()
    {
        None | Some("") | Some("Regtest") => Ok(NetworkKind::Regtest),
        Some("Mainnet") => Ok(NetworkKind::Mainnet),
        Some("Testnet") => Ok(NetworkKind::Testnet),
        Some(other) => Err(color_eyre::eyre::eyre!(
            "unrecognised {}: {other:?} (expected Mainnet, Testnet, or Regtest)",
            super::ZEBRA_TEST_ZCASHD_COMPAT_NETWORK,
        )),
    }
}
