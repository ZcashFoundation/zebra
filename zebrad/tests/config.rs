//! Integration tests for config loading via config-rs.
//!
//! Verifies layered configuration (defaults, TOML file, env) and
//! `ZEBRA_`-prefixed environment variable mappings used in Docker or
//! other environments.

use std::{env, fs, io::Write, path::PathBuf, sync::Mutex};

use tempfile::{Builder, TempDir};
use zebrad::config::ZebradConfig;

// Global mutex to ensure tests run sequentially to avoid env var races
static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Helper to isolate and manage ZEBRA_* environment variables in tests.
struct EnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original_vars: Vec<(String, String)>,
}

impl EnvGuard {
    /// Acquire the global lock and clear all ZEBRA_* env vars, saving originals.
    fn new() -> Self {
        // If a test panics, the mutex guard is dropped, but the mutex remains poisoned.
        // We can recover from the poison error and get the lock, because we're going
        // to overwrite the environment variables anyway.
        let guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let original_vars: Vec<(String, String)> = env::vars()
            .filter(|(key, _val)| key.starts_with("ZEBRA_"))
            .collect();

        for (key, _) in &original_vars {
            env::remove_var(key);
        }

        Self {
            _guard: guard,
            original_vars,
        }
    }

    /// Set a ZEBRA_* environment variable for this test.
    fn set_var(&self, key: &str, value: &str) {
        env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Clear any ZEBRA_* set during the test
        let current_vars: Vec<String> = env::vars()
            .filter(|(key, _)| key.starts_with("ZEBRA_"))
            .map(|(key, _)| key)
            .collect();
        for key in current_vars {
            env::remove_var(&key);
        }

        // Restore originals
        for (key, value) in &self.original_vars {
            env::set_var(key, value);
        }
    }
}

// --- Defaults and file loading ---

#[test]
fn config_load_defaults() {
    let _env = EnvGuard::new();

    let config = ZebradConfig::load(None).expect("Should load default config");

    assert_eq!(config.network.network.to_string(), "Mainnet");
    assert_eq!(config.rpc.listen_addr, None); // RPC disabled by default
    assert_eq!(config.metrics.endpoint_addr, None); // Metrics disabled by default
}

#[test]
fn config_load_from_file() {
    let _env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    let test_config = r#"
[network]
network = "Testnet"

[rpc]
listen_addr = "127.0.0.1:8232"

[metrics]
endpoint_addr = "127.0.0.1:9999"
"#;

    fs::write(&config_path, test_config).expect("write test config");

    let config = ZebradConfig::load(Some(config_path)).expect("load config from file");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
    assert_eq!(
        config.metrics.endpoint_addr.unwrap().to_string(),
        "127.0.0.1:9999"
    );
}

#[test]
fn config_nonexistent_file_errors() {
    let _env = EnvGuard::new();

    let nonexistent_path = PathBuf::from("/this/path/does/not/exist.toml");
    let result = ZebradConfig::load(Some(nonexistent_path));
    assert!(result.is_err(), "Should fail to load nonexistent file");
}

// --- Environment variable precedence ---

#[test]
fn config_env_override_defaults() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(None).expect("load config with env vars");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

#[test]
fn config_env_override_file() {
    let env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    let test_config = r#"
[network]
network = "Mainnet"

[rpc]
listen_addr = "127.0.0.1:8233"
"#;

    fs::write(&config_path, test_config).expect("write test config");

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(Some(config_path)).expect("load config");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

#[test]
fn config_invalid_toml_errors() {
    let _env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("invalid_config.toml");

    let invalid_config = r#"
[network
network = "Testnet"
"#;

    fs::write(&config_path, invalid_config).expect("write invalid config");

    let result = ZebradConfig::load(Some(config_path));
    assert!(result.is_err(), "Should fail to load invalid TOML");
}

#[test]
fn config_invalid_env_values_error() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "invalid_address");

    let result = ZebradConfig::load(None);
    assert!(
        result.is_err(),
        "Should fail with invalid RPC listen address"
    );
}

#[test]
fn config_nested_env_vars() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_TRACING__FILTER", "debug");

    let config = ZebradConfig::load(None).expect("load config with nested env vars");

    assert_eq!(config.tracing.filter.as_ref(), Some("debug"));
}

// --- Specific env mappings used in Docker examples ---

#[test]
fn config_zebra_network_network_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_NETWORK__NETWORK");
    assert_eq!(config.network.network.to_string(), "Testnet");
}

#[test]
fn config_zebra_rpc_listen_addr_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:18232");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_RPC__LISTEN_ADDR");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:18232"
    );
}

#[test]
fn config_zebra_state_cache_dir_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_STATE__CACHE_DIR", "/test/cache");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_STATE__CACHE_DIR");
    assert_eq!(config.state.cache_dir, PathBuf::from("/test/cache"));
}

#[test]
fn config_zebra_metrics_endpoint_addr_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_METRICS__ENDPOINT_ADDR", "0.0.0.0:9999");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_METRICS__ENDPOINT_ADDR");
    assert_eq!(
        config.metrics.endpoint_addr.unwrap().to_string(),
        "0.0.0.0:9999"
    );
}

#[test]
fn config_zebra_tracing_log_file_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_TRACING__LOG_FILE", "/test/zebra.log");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_TRACING__LOG_FILE");
    assert_eq!(
        config.tracing.log_file.as_ref().unwrap(),
        &PathBuf::from("/test/zebra.log")
    );
}

#[test]
fn config_zebra_mining_miner_address_from_toml() {
    let _env = EnvGuard::new();

    let miner_address = "u1cymdny2u2vllkx7t5jnelp0kde0dgnwu0jzmggzguxvxj6fe7gpuqehywejndlrjwgk9snr6g69azs8jfet78s9zy60uepx6tltk7ee57jlax49dezkhkgvjy2puuue6dvaevt53nah7t2cc2k4p0h0jxmlu9sx58m2xdm5f9sy2n89jdf8llflvtml2ll43e334avu2fwytuna404a";
    let toml_string = format!(
        r#"[network]
        network = "Testnet"
        
        [mining]
        miner_address = "{miner_address}""#,
    );

    let mut file = Builder::new()
        .suffix(".toml")
        .tempfile()
        .expect("create temp file");
    file.write_all(toml_string.as_bytes())
        .expect("write temp file");

    let config = ZebradConfig::load(Some(file.path().to_path_buf()))
        .expect("load config with miner_address");

    assert_eq!(
        config.mining.miner_address.as_ref().unwrap().to_string(),
        miner_address
    );
}

// --- Sensitive env deny-list behaviour ---

#[test]
fn config_env_unknown_non_sensitive_key_errors() {
    let env = EnvGuard::new();

    // Unknown field without sensitive suffix should cause an error
    env.set_var("ZEBRA_MINING__FOO", "bar");

    let result = ZebradConfig::load(None);
    assert!(
        result.is_err(),
        "Unknown non-sensitive env key should error (deny_unknown_fields)"
    );
}

#[test]
fn config_env_unknown_sensitive_key_errors() {
    let env = EnvGuard::new();

    // Unknown field with sensitive suffix should cause an error
    env.set_var("ZEBRA_MINING__TOKEN", "secret-token");

    let result = ZebradConfig::load(None);
    assert!(result.is_err(), "Sensitive env key should cause an error");
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("sensitive key"), "error message: {}", msg);
}

#[test]
fn config_env_elasticsearch_password_errors() {
    let env = EnvGuard::new();

    // This key may or may not exist depending on features. It should be filtered regardless.
    env.set_var("ZEBRA_STATE__ELASTICSEARCH_PASSWORD", "topsecret");

    let result = ZebradConfig::load(None);
    assert!(result.is_err(), "Sensitive env key should cause an error");

    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("sensitive key"), "error message: {}", msg);
}
