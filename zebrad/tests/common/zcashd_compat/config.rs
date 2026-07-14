//! Config building for the zcashd-compat integration test suite.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use color_eyre::eyre::Result;
use zebra_chain::parameters::{testnet::ConfiguredActivationHeights, Network, NetworkKind};
use zebra_rpc::config::mining::MinerAddressType;
use zebra_test::net::random_known_port;
use zebrad::{
    components::{mempool, With},
    config::ZebradConfig,
};

use super::TEST_ZCASHD_PATH;
use crate::common::config::default_test_config;

/// Configuration produced by [`build_zcashd_compat_config`].
pub struct ZcashdCompatConfig {
    pub zebrad_config: ZebradConfig,
    /// Zcashd datadir prepared for managed regtest mode.
    pub zcashd_datadir: PathBuf,
    /// Zebrad's main (unauthenticated) RPC listen address.
    pub zebra_rpc_addr: SocketAddr,
    /// Zcashd's own RPC listen address (user/pass authenticated).
    pub zcashd_own_rpc_addr: SocketAddr,
}

/// Hardcoded test credentials injected into zcashd via `-rpcuser`/`-rpcpassword`.
pub const ZCASHD_TEST_RPC_USER: &str = "zcashd_test";
pub const ZCASHD_TEST_RPC_PASS: &str = "zebra_test_pass";

/// Deterministic regtest miner keypair (secp256k1 secret key = 1, compressed).
///
/// zebrad mines coinbase to this address; tx-flow tests import the private key
/// into zcashd's wallet so the mined funds become spendable there.
pub const MINER_T_ADDR: &str = "tmLPctKo9j49rtCSKpwEBpLBeykiTGomGQs";
pub const MINER_PRIV_WIF: &str = "cMahea7zqjxrtgAbB7LSGbcQUr1uX1ojuat9jZodMN87JcbXMTcA";

/// Builds a regtest zebrad config wired for zcashd-compat testing.
///
/// `work_dir` is the test scratch directory used for the supervised zcashd
/// datadir. In managed-spawn mode this is the testdir (kept alive by the
/// `TestChild`).
pub fn build_zcashd_compat_config(work_dir: PathBuf) -> Result<ZcashdCompatConfig> {
    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );

    let zebra_rpc_port = random_known_port();
    let zcashd_own_rpc_port = random_known_port();

    let zebra_rpc_addr: SocketAddr = format!("127.0.0.1:{zebra_rpc_port}").parse()?;
    let zcashd_own_rpc_addr: SocketAddr = format!("127.0.0.1:{zcashd_own_rpc_port}").parse()?;

    let mut config = default_test_config(&net).with(MinerAddressType::Transparent);

    // Mine to the deterministic test keypair so tests can spend coinbase
    // after importing MINER_PRIV_WIF into zcashd's wallet.
    config.mining.miner_address = Some(MINER_T_ADDR.parse().expect("valid miner address"));

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
    config.zcashd_compat.zcashd_source =
        zebrad::components::zcashd_compat::ConfigZcashdBinarySource::Embedded;
    // Skip startup delay in tests — supervisor spawns zcashd immediately
    config.zcashd_compat.startup_delay = Duration::ZERO;

    // Use a fresh datadir inside the testdir. The supervisor bootstraps the
    // datadir and minimal `zcash.conf` before spawning zcashd.
    let zcashd_datadir = work_dir.join("zcashd-datadir");
    config.zcashd_compat.zcashd_datadir = Some(zcashd_datadir.clone());

    // Use an explicit zcashd path if provided, else embedded download.
    // An empty value counts as unset (the make targets always export the var).
    if let Some(path) = std::env::var_os(TEST_ZCASHD_PATH).filter(|path| !path.is_empty()) {
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
        // Match zebrad's regtest activation heights (NU5 at height 1), or
        // zcashd rejects zebrad's mined blocks with `AcceptBlock FAILED`.
        "-nuparams=5ba81b19:1".to_string(), // Overwinter
        "-nuparams=76b809bb:1".to_string(), // Sapling
        "-nuparams=2bb40e60:1".to_string(), // Blossom
        "-nuparams=f5b9230b:1".to_string(), // Heartwood
        "-nuparams=e9ff75a6:1".to_string(), // Canopy
        "-nuparams=c2d6d0b4:1".to_string(), // NU5
        // The wallet tests use `getnewaddress`, which is deny-by-default
        // deprecated in current zcashd.
        "-allowdeprecated=getnewaddress".to_string(),
        // Regtest blocks mined on top of the 2011 genesis inherit old
        // median-time-past timestamps, which would keep zcashd in initial
        // block download forever and disable its wallet RPCs. 100 years.
        "-maxtipage=3153600000".to_string(),
    ];

    Ok(ZcashdCompatConfig {
        zebrad_config: config,
        zcashd_datadir,
        zebra_rpc_addr,
        zcashd_own_rpc_addr,
    })
}

/// Returns the expected zebrad `chain` field value for the given network.
pub fn expected_zebrad_chain_name(network: &Network) -> String {
    network.bip70_network_name()
}

/// Returns the expected zcashd `chain` field value for the given network.
pub fn expected_zcashd_chain_name(network: &Network) -> &'static str {
    match network.kind() {
        NetworkKind::Mainnet => "main",
        NetworkKind::Testnet => "test",
        NetworkKind::Regtest => "regtest",
    }
}

/// Reads `TEST_ZCASHD_COMPAT_NETWORK` and returns the corresponding
/// [`NetworkKind`].  Defaults to `Regtest` when absent.
///
/// Returns `Err` for unrecognised values.
pub fn read_test_network_kind() -> Result<NetworkKind> {
    match std::env::var(super::TEST_ZCASHD_COMPAT_NETWORK)
        .ok()
        .as_deref()
    {
        None | Some("") | Some("Regtest") => Ok(NetworkKind::Regtest),
        Some("Mainnet") => Ok(NetworkKind::Mainnet),
        Some("Testnet") => Ok(NetworkKind::Testnet),
        Some(other) => Err(color_eyre::eyre::eyre!(
            "unrecognised {}: {other:?} (expected Mainnet, Testnet, or Regtest)",
            super::TEST_ZCASHD_COMPAT_NETWORK,
        )),
    }
}
