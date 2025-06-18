//! Integration tests for Zebrad configuration loading and overrides.
//!
//! These tests verify:
//! - Correct loading of configuration from TOML files.
//! - Precedence order: Defaults < TOML < Environment Variables.
//! - Environment variable override conventions (ZEBRA_ prefix, double underscore for nesting).
//! - Error handling for invalid config files and environment variable values.
//! - Edge cases such as missing files, invalid types, and validation errors.
//!
//! See the documentation in `zebra/zebrad/src/config.rs` for details on the config system and override conventions.

use figment::Jail;
use zebrad::config::ZebradConfig;

#[test]
/// Loads a custom TOML config and checks that all fields are parsed as expected.
/// This test ensures TOML parsing and field mapping works for a non-default config.
fn test_load_custom_conf_config() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        let config_content = include_str!("common/configs/custom-conf.toml");
        let config_path = jail.directory().join("custom-conf.toml");
        jail.create_file(&config_path, config_content)?;

        let config =
            ZebradConfig::load(Some(config_path)).expect("Failed to load custom-conf.toml");

        // Verify network configuration
        assert_eq!(config.network.network.to_string(), "Mainnet");
        assert!(config.network.cache_dir.is_enabled());
        assert_eq!(config.network.listen_addr.to_string(), "0.0.0.0:8233");
        assert_eq!(config.network.max_connections_per_ip, 1);
        assert_eq!(config.network.peerset_initial_target_size, 25);

        // Verify RPC configuration
        assert!(config.rpc.enable_cookie_auth);
        assert_eq!(config.rpc.parallel_cpu_threads, 0);

        // Verify mining configuration
        assert_eq!(
            config.mining.extra_coinbase_data,
            Some("do you even shield?".to_string())
        );

        // Verify mempool configuration
        assert_eq!(config.mempool.tx_cost_limit, 80_000_000);

        Ok(())
    });
}

#[test]
/// Loads a TOML config for getblocktemplate and checks key fields.
/// Verifies TOML parsing for a config with mining and sync options.
fn test_load_getblocktemplate_config() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        let config_content = include_str!("common/configs/getblocktemplate-v1.0.1.toml");
        let config_path = jail.directory().join("getblocktemplate.toml");
        jail.create_file(&config_path, config_content)?;

        let config =
            ZebradConfig::load(Some(config_path)).expect("Failed to load getblocktemplate config");

        // Verify network configuration
        assert_eq!(config.network.network.to_string(), "Mainnet");
        assert_eq!(config.network.listen_addr.to_string(), "0.0.0.0:8233");
        assert_eq!(config.network.max_connections_per_ip, 1);

        // Verify consensus configuration
        assert!(config.consensus.checkpoint_sync);

        // Verify sync configuration
        assert_eq!(config.sync.checkpoint_verify_concurrency_limit, 1000);
        assert_eq!(config.sync.download_concurrency_limit, 50);
        assert_eq!(config.sync.full_verify_concurrency_limit, 20);

        Ok(())
    });
}

#[test]
/// Loads a TOML config from v2.4.0 and checks that defaults and overrides are correct.
fn test_load_v2_4_0_config() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        let config_content = include_str!("common/configs/v2.4.0.toml");
        let config_path = jail.directory().join("v2.4.0.toml");
        jail.create_file(&config_path, config_content)?;

        let config = ZebradConfig::load(Some(config_path)).expect("Failed to load v2.4.0 config");

        // Verify network configuration
        assert_eq!(config.network.network.to_string(), "Mainnet");
        assert_eq!(config.network.listen_addr.to_string(), "[::]:8233");
        assert!(config.network.cache_dir.is_enabled());

        // Verify RPC configuration
        assert!(config.rpc.enable_cookie_auth);

        // Verify mining configuration
        assert!(!config.mining.internal_miner);

        // Verify state configuration
        assert!(config.state.delete_old_database);
        assert!(!config.state.ephemeral);

        Ok(())
    });
}

#[test]
/// Loads a TOML config with internal miner enabled and checks mining and mempool fields.
fn test_load_internal_miner_config() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        let config_content = include_str!("common/configs/v1.9.0-internal-miner.toml");
        let config_path = jail.directory().join("internal-miner.toml");
        jail.create_file(&config_path, config_content)?;

        let config =
            ZebradConfig::load(Some(config_path)).expect("Failed to load internal miner config");

        // Verify network configuration - this config uses Testnet
        assert_eq!(config.network.network.to_string(), "Testnet");
        assert_eq!(config.network.listen_addr.to_string(), "0.0.0.0:18233");

        // Verify mining configuration - internal miner enabled
        assert!(config.mining.internal_miner);
        assert!(config.mining.miner_address.is_some());

        // Verify mempool has debug settings
        assert_eq!(config.mempool.debug_enable_at_height, Some(0));

        Ok(())
    });
}

#[test]
/// Demonstrates environment variable overrides using the ZEBRA_ prefix and double underscores for nesting.
/// This test sets env vars to override TOML config values and verifies the precedence and mapping.
fn test_figment_env_override() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        // Create a basic config file
        jail.create_file(
            "test_env.toml",
            r#"
                    [network]
                    network = "Mainnet"
                    listen_addr = "0.0.0.0:8233"

                    [rpc]
                    enable_cookie_auth = false
                    parallel_cpu_threads = 1
                "#,
        )?;

        // Set environment variables to override config values
        jail.set_env("ZEBRA_NETWORK__NETWORK", "Testnet");
        jail.set_env("ZEBRA_NETWORK__CACHE_DIR", "/tmp/zebra-cache");
        jail.set_env("ZEBRA_RPC__ENABLE_COOKIE_AUTH", "true");
        jail.set_env("ZEBRA_RPC__PARALLEL_CPU_THREADS", "4");

        let config_path = jail.directory().join("test_env.toml");
        let config = ZebradConfig::load(Some(config_path))
            .expect("Failed to load config with env overrides");

        // Verify environment variables override TOML values
        assert_eq!(config.network.network.to_string(), "Testnet"); // Overridden by ZEBRA_NETWORK__NETWORK
        assert!(config.rpc.enable_cookie_auth); // Overridden by ZEBRA_RPC__ENABLE_COOKIE_AUTH
        assert_eq!(config.rpc.parallel_cpu_threads, 4); // Overridden by ZEBRA_RPC__PARALLEL_CPU_THREADS

        // Verify custom cache_dir mapping works
        match &config.network.cache_dir {
            zebra_network::CacheDir::CustomPath(path) => {
                if let Some(path_str) = path.to_str() {
                    assert_eq!(path_str, "/tmp/zebra-cache");
                }
            }
            _ => panic!("Expected CustomPath cache_dir variant"),
        }

        Ok(())
    });
}

#[test]
/// Loads an empty TOML config and checks that all default values are used.
fn test_figment_default_config() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        jail.create_file("empty.toml", "")?;
        let config_path = jail.directory().join("empty.toml");
        let config = ZebradConfig::load(Some(config_path)).expect("Failed to load default config");

        let default_config = ZebradConfig::default();

        // Compare key default values
        assert_eq!(config.network.network, default_config.network.network);
        assert_eq!(
            config.consensus.checkpoint_sync,
            default_config.consensus.checkpoint_sync
        );
        assert_eq!(
            config.rpc.enable_cookie_auth,
            default_config.rpc.enable_cookie_auth
        );
        assert_eq!(
            config.mining.internal_miner,
            default_config.mining.internal_miner
        );

        Ok(())
    });
}

#[test]
/// Attempts to load a nonexistent config file and checks that defaults are used.
fn test_figment_nonexistent_config_file() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        let nonexistent_path = jail.directory().join("nonexistent.toml");
        let config = ZebradConfig::load(Some(nonexistent_path))
            .expect("Failed to load with nonexistent file");

        let default_config = ZebradConfig::default();

        // Should fallback to defaults
        assert_eq!(config.network.network, default_config.network.network);
        assert_eq!(
            config.consensus.checkpoint_sync,
            default_config.consensus.checkpoint_sync
        );

        Ok(())
    });
}

#[test]
/// Attempts to load a config with invalid TOML syntax and checks that an error is returned.
fn test_figment_invalid_toml() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        jail.create_file(
            "invalid.toml",
            r#"
                    [network
                    network = "Mainnet"
                    # Missing closing bracket - invalid TOML
                "#,
        )?;

        let config_path = jail.directory().join("invalid.toml");
        let result = ZebradConfig::load(Some(config_path));

        // Should return an error for invalid TOML
        assert!(result.is_err());

        Ok(())
    });
}

#[test]
/// Sets an environment variable to an invalid type for a numeric field and checks that an error is returned.
fn test_figment_invalid_env_var_type() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        jail.create_file("test_invalid_env.toml", "")?;

        // Set invalid environment variable (string for numeric field)
        jail.set_env("ZEBRA_RPC__PARALLEL_CPU_THREADS", "not_a_number");

        let config_path = jail.directory().join("test_invalid_env.toml");
        let result = ZebradConfig::load(Some(config_path));

        // Should return an error for invalid type conversion
        assert!(result.is_err());

        Ok(())
    });
}

#[test]
/// Demonstrates custom environment variable overrides for nested fields (network.network, network.cache_dir).
/// Verifies that the double underscore convention works for all nested fields.
fn test_custom_env_mappings() {
    Jail::expect_with(|jail| {
        // Clear any existing environment variables that might interfere
        jail.clear_env();

        jail.create_file(
            "test_custom_env.toml",
            r#"
                    [network]
                    network = "Mainnet"
                    cache_dir = "/default/cache"
                "#,
        )?;

        // Test custom mappings
        jail.set_env("ZEBRA_NETWORK__NETWORK", "Testnet");
        jail.set_env("ZEBRA_NETWORK__CACHE_DIR", "/custom/cache/dir");

        let config_path = jail.directory().join("test_custom_env.toml");
        let config = ZebradConfig::load(Some(config_path))
            .expect("Failed to load config with custom env mappings");

        // Verify custom mappings work
        assert_eq!(config.network.network.to_string(), "Testnet");

        match &config.network.cache_dir {
            zebra_network::CacheDir::CustomPath(path) => {
                if let Some(path_str) = path.to_str() {
                    assert_eq!(path_str, "/custom/cache/dir");
                }
            }
            _ => panic!("Expected CustomPath cache_dir variant"),
        }

        Ok(())
    });
}

#[test]
/// Calls ZebradConfig::load(None) to check that it does not panic when no config path is specified.
/// This test does not assert on the result, only that it is handled gracefully.
fn test_load_config_default_path() {
    // This test would require setting up a config file at the default path
    // For now, we'll test that the function doesn't panic when called with None
    let result = ZebradConfig::load(None);

    // The result might be Ok (if default config exists) or Err (if it doesn't)
    // Either way, it shouldn't panic
    match result {
        Ok(_config) => {
            // Config loaded successfully from default path
        }
        Err(_error) => {
            // Config file not found at default path, which is expected in test environment
        }
    }
}

#[test]
/// Sets an invalid miner address via environment variable and checks that validation fails with an error.
fn test_invalid_miner_address_fails() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        // Set environment variable with an invalid address
        jail.set_env("ZEBRA_MINING__MINER_ADDRESS", "this-is-not-a-valid-address");

        let config_path = jail.directory().join("invalid_address.toml");
        let result = ZebradConfig::load(Some(config_path));

        // Should return an error for the invalid address
        assert!(result.is_err());

        if let Err(e) = result {
            assert!(e.to_string().contains("MINING.MINER_ADDRESS"));
        }

        Ok(())
    });
}

#[test]
/// Replicates the CI 'Test RPC config' failure.
/// Verifies that setting ZEBRA_RPC__LISTEN_ADDR via environment variables works correctly.
fn test_rpc_config_from_env() {
    Jail::expect_with(|jail| {
        // Clear any existing ZEBRA_* environment variables that might interfere
        jail.clear_env();

        // This is what `prepare_conf_file` in entrypoint.sh does when ZEBRA_RPC_PORT is set.
        jail.set_env("ZEBRA_RPC__LISTEN_ADDR", "0.0.0.0:8232");

        // Load config with no config file, relying only on defaults and env vars.
        let config = ZebradConfig::load(None)
            .expect("Failed to load config with RPC env vars");

        // Verify that the RPC listen address is correctly parsed and set
        assert_eq!(
            config.rpc.listen_addr.as_ref().map(|addr| addr.to_string()),
            Some("0.0.0.0:8232".to_string())
        );

        Ok(())
    });
}
