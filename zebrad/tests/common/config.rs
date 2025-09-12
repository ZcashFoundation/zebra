//! `zebrad` config-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::Result;
use tempfile::TempDir;

use zebra_chain::parameters::{Network, NetworkKind};
use zebra_test::{command::TestChild, net::random_known_port};
use zebrad::{
    components::{mempool, sync, tracing},
    config::ZebradConfig,
};

use crate::common::cached_state::DATABASE_FORMAT_CHECK_INTERVAL;

/// Returns a config with:
/// - a Zcash listener on an unused port on IPv4 localhost, and
/// - an ephemeral state,
/// - the minimum syncer lookahead limit, and
/// - shorter task intervals, to improve test coverage.
pub fn default_test_config(net: &Network) -> Result<ZebradConfig> {
    const TEST_DURATION: Duration = Duration::from_secs(30);

    let network = zebra_network::Config {
        network: net.clone(),
        // The OS automatically chooses an unused port.
        listen_addr: "127.0.0.1:0".parse()?,
        crawl_new_peer_interval: TEST_DURATION,
        ..zebra_network::Config::default()
    };

    let sync = sync::Config {
        // Avoid downloading unnecessary blocks.
        checkpoint_verify_concurrency_limit: sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
        ..sync::Config::default()
    };

    let mempool = mempool::Config {
        eviction_memory_time: TEST_DURATION,
        ..mempool::Config::default()
    };

    let consensus = zebra_consensus::Config::default();

    let force_use_color = env::var("FORCE_USE_COLOR").is_ok();

    let mut tracing = tracing::Config::default();
    tracing.force_use_color = force_use_color;

    let mut state = zebra_state::Config::ephemeral();
    state.debug_validity_check_interval = Some(DATABASE_FORMAT_CHECK_INTERVAL);

    // Provide a default miner address matched to the effective network so
    // mining-related tests have a valid configuration. Environment variables
    // like ZEBRA_MINING__MINER_ADDRESS will override this file value.
    let mut mining = zebra_rpc::config::mining::Config::default();

    // Determine the effective network kind: prefer environment override if present.
    let effective_kind: NetworkKind = match env::var("ZEBRA_NETWORK__NETWORK").ok().as_deref() {
        Some("Mainnet") => NetworkKind::Mainnet,
        Some("Regtest") => NetworkKind::Regtest,
        Some("Testnet") => NetworkKind::Testnet,
        _ => net.kind(),
    };

    let default_miner_address = match effective_kind {
        // Mainnet UA
        NetworkKind::Mainnet => "u1cymdny2u2vllkx7t5jnelp0kde0dgnwu0jzmggzguxvxj6fe7gpuqehywejndlrjwgk9snr6g69azs8jfet78s9zy60uepx6tltk7ee57jlax49dezkhkgvjy2puuue6dvaevt53nah7t2cc2k4p0h0jxmlu9sx58m2xdm5f9sy2n89jdf8llflvtml2ll43e334avu2fwytuna404a",
        // Regtest UA
        NetworkKind::Regtest => "uregtest1a2yn922nnxyvnj4qmax07lkr7kmnyxq3rw0paa2kes87h2rapehrzgy8xrq665sg6aatmpgzkngwlumzr40e5y4vc40a809rsyqcwq25xfj5r2sxu774xdt6dj5xckjkv5ll0c2tv6qtsl60mpccwd6m95upy2da0rheqmkmxr7fv9z5uve0kpkmssxcuvzasewwns986yud6aact4y",
        // Testnet UA
        NetworkKind::Testnet => "utest1quxrs9munape90f833rnse9s02xwkvrh2yzlvm56rsg2lccpr3kwmprxw4zq6ukkv5ht6uvmzasf9pwwfhpfqct4ghmkp7zka6ufurnc9vkwvzt4jved8hld2cram6x75qxs0dgg3eq8gef8kttpw4eqjywnxpuns0fpfz072whje4xmld6ahy9dezsvzmugemn8lerr47lhcx3rzl6",
    };

    mining.miner_address = Some(
        default_miner_address
            .parse()
            .expect("hard-coded address is valid"),
    );

    Ok(ZebradConfig {
        network,
        state,
        sync,
        mempool,
        consensus,
        tracing,
        mining,
        ..ZebradConfig::default()
    })
}

pub fn persistent_test_config(network: &Network) -> Result<ZebradConfig> {
    let mut config = default_test_config(network)?;
    config.state.ephemeral = false;
    Ok(config)
}

pub fn external_address_test_config(network: &Network) -> Result<ZebradConfig> {
    let mut config = default_test_config(network)?;
    config.network.external_addr = Some("127.0.0.1:0".parse()?);
    Ok(config)
}

pub fn testdir() -> Result<TempDir> {
    tempfile::Builder::new()
        .prefix("zebrad_tests")
        .tempdir()
        .map_err(Into::into)
}

/// Get the directory where we have different config files.
pub fn configs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/configs")
}

/// Given a config file name, return full path to it.
pub fn config_file_full_path(config_file: PathBuf) -> PathBuf {
    let path = configs_dir().join(config_file);
    Path::new(&path).into()
}

/// Returns a `zebrad` config with a random known RPC port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
pub fn random_known_rpc_port_config(
    parallel_cpu_threads: bool,
    network: &Network,
) -> Result<ZebradConfig> {
    // [Note on port conflict](#Note on port conflict)
    let listen_port = random_known_port();
    rpc_port_config(listen_port, parallel_cpu_threads, network)
}

/// Returns a `zebrad` config with an OS-assigned RPC port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
pub fn os_assigned_rpc_port_config(
    parallel_cpu_threads: bool,
    network: &Network,
) -> Result<ZebradConfig> {
    rpc_port_config(0, parallel_cpu_threads, network)
}

/// Returns a `zebrad` config with the provided RPC port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
pub fn rpc_port_config(
    listen_port: u16,
    parallel_cpu_threads: bool,
    network: &Network,
) -> Result<ZebradConfig> {
    let listen_ip = "127.0.0.1".parse().expect("hard-coded IP is valid");
    let zebra_rpc_listener = SocketAddr::new(listen_ip, listen_port);

    // Write a configuration that has the rpc listen_addr option set
    // TODO: split this config into another function?
    let mut config = default_test_config(network)?;
    config.rpc.listen_addr = Some(zebra_rpc_listener);
    if parallel_cpu_threads {
        // Auto-configure to the number of CPU cores: most users configure this
        config.rpc.parallel_cpu_threads = 0;
    } else {
        // Default config, users who want to detect port conflicts configure this
        config.rpc.parallel_cpu_threads = 1;
    }
    config.rpc.enable_cookie_auth = false;

    Ok(config)
}

/// Reads Zebra's RPC server listen address from a testchild's logs
pub fn read_listen_addr_from_logs(
    child: &mut TestChild<TempDir>,
    expected_msg: &str,
) -> Result<SocketAddr> {
    let line = child.expect_stdout_line_matches(expected_msg)?;
    let rpc_addr_position =
        line.find(expected_msg).expect("already checked for match") + expected_msg.len();
    let rpc_addr = line[rpc_addr_position..].trim().to_string();
    Ok(rpc_addr.parse()?)
}
