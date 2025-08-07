//! Tests for Docker environment variable integration
//!
//! These tests verify that the ZEBRA_ prefixed environment variables are
//! properly handled by the config-rs system.

use std::{env, io::Write, sync::Mutex};
use tempfile::Builder;
use zebrad::config::ZebradConfig;

// Global mutex to ensure tests run sequentially and don't interfere with each other
static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Helper struct to manage environment variables in tests
struct EnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    test_vars: Vec<String>,
}

impl EnvGuard {
    /// Create a new EnvGuard, clearing relevant environment variables
    fn new() -> Self {
        let guard = match TEST_MUTEX.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        Self {
            _guard: guard,
            test_vars: Vec::new(),
        }
    }

    /// Set an environment variable for the test
    fn set_var(&mut self, key: &str, value: &str) {
        env::set_var(key, value);
        self.test_vars.push(key.to_string());
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for key in &self.test_vars {
            env::remove_var(key);
        }
    }
}

/// Test that ZEBRA_NETWORK__NETWORK is mapped correctly
#[test]
fn test_zebra_network_network() {
    let mut env_guard = EnvGuard::new();

    env_guard.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");

    let config = ZebradConfig::load(None).expect("Should load config with ZEBRA_NETWORK__NETWORK");

    assert_eq!(config.network.network.to_string(), "Testnet");
}

/// Test that ZEBRA_RPC__LISTEN_ADDR is mapped correctly
#[test]
fn test_zebra_rpc_listen_addr() {
    let mut env_guard = EnvGuard::new();

    env_guard.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:18232");

    let config = ZebradConfig::load(None).expect("Should load config with ZEBRA_RPC__LISTEN_ADDR");

    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:18232"
    );
}

/// Test that ZEBRA_STATE__CACHE_DIR is mapped correctly
#[test]
fn test_zebra_state_cache_dir() {
    let mut env_guard = EnvGuard::new();

    env_guard.set_var("ZEBRA_STATE__CACHE_DIR", "/test/cache");

    let config = ZebradConfig::load(None).expect("Should load config with ZEBRA_STATE__CACHE_DIR");

    assert_eq!(
        config.state.cache_dir,
        std::path::PathBuf::from("/test/cache")
    );
}

/// Test that ZEBRA_METRICS__ENDPOINT_ADDR is mapped correctly
#[test]
fn test_zebra_metrics_endpoint_addr() {
    let mut env_guard = EnvGuard::new();

    env_guard.set_var("ZEBRA_METRICS__ENDPOINT_ADDR", "0.0.0.0:9999");

    let config =
        ZebradConfig::load(None).expect("Should load config with ZEBRA_METRICS__ENDPOINT_ADDR");

    assert_eq!(
        config.metrics.endpoint_addr.unwrap().to_string(),
        "0.0.0.0:9999"
    );
}

/// Test that ZEBRA_TRACING__LOG_FILE is mapped correctly
#[test]
fn test_zebra_tracing_log_file() {
    let mut env_guard = EnvGuard::new();

    env_guard.set_var("ZEBRA_TRACING__LOG_FILE", "/test/zebra.log");

    let config = ZebradConfig::load(None).expect("Should load config with ZEBRA_TRACING__LOG_FILE");

    assert_eq!(
        config.tracing.log_file.as_ref().unwrap(),
        &std::path::PathBuf::from("/test/zebra.log")
    );
}

/// Test that a miner address in a TOML file is mapped correctly
#[test]
fn test_zebra_mining_miner_address() {
    let miner_address = "u1cymdny2u2vllkx7t5jnelp0kde0dgnwu0jzmggzguxvxj6fe7gpuqehywejndlrjwgk9snr6g69azs8jfet78s9zy60uepx6tltk7ee57jlax49dezkhkgvjy2puuue6dvaevt53nah7t2cc2k4p0h0jxmlu9sx58m2xdm5f9sy2n89jdf8llflvtml2ll43e334avu2fwytuna404a";
    let toml_string = format!(
        "[network]\nnetwork = \"Testnet\"\n\n[mining]\nminer_address = \"{}\"",
        miner_address
    );

    let mut file = Builder::new()
        .suffix(".toml")
        .tempfile()
        .expect("Should create a new temp file");
    file.write_all(toml_string.as_bytes())
        .expect("Should write to temp file");

    let config = ZebradConfig::load(Some(file.path().to_path_buf()))
        .expect("Should load config with miner_address");

    assert_eq!(
        config.mining.miner_address.as_ref().unwrap().to_string(),
        miner_address
    );
}
