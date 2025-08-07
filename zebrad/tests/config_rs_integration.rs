//! Tests for config-rs integration
//!
//! These tests verify that the new config-rs based configuration loading works correctly.
//!
//! Note: These tests use a mutex to ensure they run sequentially to avoid
//! environment variable contamination between tests.

use std::{env, fs, sync::Mutex};
use tempfile::TempDir;
use zebrad::config::ZebradConfig;

// Global mutex to ensure tests run sequentially and don't interfere with each other
static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Helper struct to manage environment variables in tests
struct EnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original_vars: Vec<(String, String)>,
}

impl EnvGuard {
    /// Create a new EnvGuard, clearing all ZEBRA_ environment variables
    fn new() -> Self {
        let guard = TEST_MUTEX.lock().unwrap();

        let original_vars: Vec<(String, String)> = env::vars()
            .filter(|(key, _)| key.starts_with("ZEBRA_"))
            .collect();

        // Clear all ZEBRA_ environment variables
        for (key, _) in &original_vars {
            env::remove_var(key);
        }

        Self {
            _guard: guard,
            original_vars,
        }
    }

    /// Set a ZEBRA_ environment variable for testing
    fn set_var(&self, key: &str, value: &str) {
        env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Clear any ZEBRA_ vars that might have been set during the test
        let current_vars: Vec<String> = env::vars()
            .filter(|(key, _)| key.starts_with("ZEBRA_"))
            .map(|(key, _)| key)
            .collect();

        for key in current_vars {
            env::remove_var(&key);
        }

        // Restore original environment variables
        for (key, value) in &self.original_vars {
            env::set_var(key, value);
        }
    }
}

/// Test that loading a config with defaults works
#[test]
fn test_config_load_defaults() {
    let _env_guard = EnvGuard::new();

    let config = ZebradConfig::load(None).expect("Should load default config");

    // Verify some basic defaults
    assert_eq!(config.network.network.to_string(), "Mainnet");
    assert_eq!(config.rpc.listen_addr, None); // RPC disabled by default
    assert_eq!(config.metrics.endpoint_addr, None); // Metrics disabled by default
}

/// Test that loading a config from a TOML file works
#[test]
fn test_config_load_from_file() {
    let _env_guard = EnvGuard::new();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    // Create a simple test config
    let test_config = r#"
[network]
network = "Testnet"

[rpc]
listen_addr = "127.0.0.1:8232"

[metrics]
endpoint_addr = "127.0.0.1:9999"
"#;

    fs::write(&config_path, test_config).expect("Failed to write test config");

    let config = ZebradConfig::load(Some(config_path)).expect("Should load config from file");

    // Verify the values from the file were loaded
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

/// Test that environment variables override defaults
#[test]
fn test_config_env_override_defaults() {
    let env_guard = EnvGuard::new();

    // Set environment variables
    env_guard.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env_guard.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(None).expect("Should load config with env vars");

    // Verify environment variables overrode defaults
    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

/// Test that environment variables override TOML file values
#[test]
fn test_config_env_override_file() {
    let env_guard = EnvGuard::new();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    // Create a test config file
    let test_config = r#"
[network]
network = "Mainnet"

[rpc]
listen_addr = "127.0.0.1:8233"
"#;

    fs::write(&config_path, test_config).expect("Failed to write test config");

    // Set environment variables that should override the file
    env_guard.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env_guard.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(Some(config_path)).expect("Should load config");

    // Verify environment variables overrode file values
    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

/// Test that invalid TOML files produce appropriate errors
#[test]
fn test_config_invalid_toml() {
    let _env_guard = EnvGuard::new();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("invalid_config.toml");

    // Create an invalid TOML file (missing closing bracket)
    let invalid_config = r#"
[network
network = "Testnet"
"#;

    fs::write(&config_path, invalid_config).expect("Failed to write invalid config");

    let result = ZebradConfig::load(Some(config_path));
    assert!(result.is_err(), "Should fail to load invalid TOML");
}

/// Test that invalid environment variable values produce appropriate errors
#[test]
fn test_config_invalid_env_values() {
    let env_guard = EnvGuard::new();

    // Set an invalid RPC listen address (should be a socket address)
    env_guard.set_var("ZEBRA_RPC__LISTEN_ADDR", "invalid_address");

    let result = ZebradConfig::load(None);
    assert!(
        result.is_err(),
        "Should fail with invalid RPC listen address"
    );
}

/// Test that nested environment variables work correctly
#[test]
fn test_config_nested_env_vars() {
    let env_guard = EnvGuard::new();

    // Test nested configuration like tracing.filter
    env_guard.set_var("ZEBRA_TRACING__FILTER", "debug");

    let config = ZebradConfig::load(None).expect("Should load config with nested env vars");

    // Verify the nested value was set
    assert_eq!(config.tracing.filter.as_ref().unwrap(), "debug");
}

/// Test that loading nonexistent file falls back to defaults
#[test]
fn test_config_nonexistent_file() {
    let _env_guard = EnvGuard::new();

    let nonexistent_path = std::path::PathBuf::from("/this/path/does/not/exist.toml");

    let result = ZebradConfig::load(Some(nonexistent_path));
    assert!(result.is_err(), "Should fail to load nonexistent file");
}
