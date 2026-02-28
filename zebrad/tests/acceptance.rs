//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! ## Note on port conflict
//!
//! If the test child has a cache or port conflict with another test, or a
//! running zebrad or zcashd, then it will panic. But the acceptance tests
//! expect it to run until it is killed.
//!
//! If these conflicts cause test failures:
//!   - run the tests in an isolated environment,
//!   - run zebrad on a custom cache path and port,
//!   - run zcashd on a custom port.
//!
//! ## Failures due to Configured Network Interfaces or Network Connectivity
//!
//! If your test environment does not have any IPv6 interfaces configured, skip IPv6 tests
//! by setting the `SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, IPv4 localhost is not on `127.0.0.1`,
//! or you have poor network connectivity,
//! skip all the network tests by setting the `SKIP_NETWORK_TESTS` environmental variable.
//!
//! ## Large/full sync tests
//!
//! This file has sync tests that are marked as ignored because they take too much time to run.
//! Some of them require environment variables or directories to be present:
//!
//! - `SYNC_FULL_MAINNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//!   will allow this test to run or give up. Value for the Mainnet full sync tests.
//! - `SYNC_FULL_TESTNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//!   will allow this test to run or give up. Value for the Testnet full sync tests.
//! - A zebrad state cache directory is required for some tests, either at the default state cache
//!   directory path, or at the path defined in the `ZEBRA_STATE__CACHE_DIR` env variable.
//!   For some sync tests, this directory needs to be created in the file system
//!   with write permissions.
//!
//! Here are some examples on how to run each of the tests using nextest profiles:
//!
//! ```console
//! $ cargo nextest run --profile sync-large-checkpoints-empty
//!
//! $ ZEBRA_STATE__CACHE_DIR="/zebrad-cache" cargo nextest run --profile sync-full-mainnet
//!
//! $ ZEBRA_STATE__CACHE_DIR="/zebrad-cache" cargo nextest run --profile sync-full-testnet
//! ```
//!
//! For tests that require a cache directory, you may need to create it first:
//! ```console
//! $ export ZEBRA_STATE__CACHE_DIR="/zebrad-cache"
//! $ sudo mkdir -p "$ZEBRA_STATE__CACHE_DIR"
//! $ sudo chmod 777 "$ZEBRA_STATE__CACHE_DIR"
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Lightwalletd tests
//!
//! The lightwalletd software is an interface service that uses zebrad or zcashd RPC methods to serve wallets or other applications with blockchain content in an efficient manner.
//!
//! Zebra's lightwalletd tests are executed using nextest profiles.
//! Some tests require environment variables to be set:
//!
//! - `TEST_LIGHTWALLETD`: Must be set to run any of the lightwalletd tests.
//! - `ZEBRA_STATE__CACHE_DIR`: The path to a Zebra cached state directory.
//! - `LWD_CACHE_DIR`: The path to a lightwalletd database.
//!
//! Here are some examples of running each test:
//!
//! ```console
//! # Run the lightwalletd integration test
//! $ TEST_LIGHTWALLETD=1 cargo nextest run --profile lwd-integration
//!
//! # Run the lightwalletd update sync test
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-sync-update
//!
//! # Run the lightwalletd full sync test
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" cargo nextest run --profile lwd-sync-full
//!
//! # Run the lightwalletd gRPC wallet test (requires --features lightwalletd-grpc-tests)
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-grpc-wallet --features lightwalletd-grpc-tests
//!
//! # Run the lightwalletd send transaction test (requires --features lightwalletd-grpc-tests)
//! $ TEST_LIGHTWALLETD=1 ZEBRA_STATE__CACHE_DIR="/path/to/zebra/state" LWD_CACHE_DIR="/path/to/lightwalletd/database" cargo nextest run --profile lwd-rpc-send-tx --features lightwalletd-grpc-tests
//! ```
//!
//! ## Getblocktemplate tests
//!
//! Example of how to run the rpc_get_block_template test:
//!
//! ```console
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile rpc-get-block-template
//! ```
//!
//! Example of how to run the rpc_submit_block test:
//!
//! ```console
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile rpc-submit-block
//! ```
//!
//! Example of how to run the has_spending_transaction_ids test (requires indexer feature):
//!
//! ```console
//! RUST_LOG=info ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile indexer-has-spending-transaction-ids --features "indexer"
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Checkpoint Generation Tests
//!
//! Generate checkpoints on mainnet and testnet using a cached state:
//! ```console
//! # Generate checkpoints for mainnet:
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile generate-checkpoints-mainnet
//!
//! # Generate checkpoints for testnet:
//! ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state cargo nextest run --profile generate-checkpoints-testnet
//! ```
//!
//! You can also use the entrypoint script directly:
//! ```console
//! FEATURES=zebra-checkpoints ZEBRA_STATE__CACHE_DIR=/path/to/zebra/state docker/entrypoint.sh
//! ```
//!
//! ## Disk Space for Testing
//!
//! The full sync and lightwalletd tests with cached state expect a temporary directory with
//! at least 300 GB of disk space (2 copies of the full chain). To use another disk for the
//! temporary test files:
//!
//! ```sh
//! export TMPDIR=/path/to/disk/directory
//! ```

#![allow(clippy::unwrap_in_result)]

mod common;

use std::{
    cmp::Ordering,
    collections::HashSet,
    env, fs, panic,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::{
    eyre::{eyre, WrapErr},
    Help,
};
use futures::{stream::FuturesUnordered, StreamExt};
use semver::Version;
use serde_json::Value;
use tower::ServiceExt;

use zcash_keys::address::Address;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network::{self, *},
        NetworkUpgrade,
    },
    serialization::BytesInDisplayOrder,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{
        BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateRequestMode,
        GetBlockTemplateResponse, SubmitBlockErrorResponse, SubmitBlockResponse,
    },
    fetch_state_tip_and_local_time, generate_coinbase_and_roots,
    methods::{RpcImpl, RpcServer},
    proposal_block_from_template,
    server::OPENED_RPC_ENDPOINT_MSG,
    SubmitBlockChannel,
};
use zebra_state::{constants::LOCK_FILE_ERROR, state_database_format_version_in_code};
use zebra_test::{
    args,
    command::{to_regex::CollectRegexSet, ContextFrom},
    net::random_known_port,
    prelude::*,
};

#[cfg(not(target_os = "windows"))]
use zebra_network::constants::PORT_IN_USE_ERROR;

use common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    check::{is_zebrad_version, EphemeralCheck, EphemeralConfig},
    config::{
        config_file_full_path, configs_dir, default_test_config, external_address_test_config,
        os_assigned_rpc_port_config, persistent_test_config, random_known_rpc_port_config,
        read_listen_addr_from_logs, testdir,
    },
    launch::{
        spawn_zebrad_for_rpc, spawn_zebrad_without_rpc, ZebradTestDirExt, BETWEEN_NODES_DELAY,
        EXTENDED_LAUNCH_DELAY, LAUNCH_DELAY,
    },
    lightwalletd::{can_spawn_lightwalletd_for_rpc, spawn_lightwalletd_for_rpc},
    sync::{
        create_cached_database_height, sync_until, MempoolBehavior, LARGE_CHECKPOINT_TEST_HEIGHT,
        LARGE_CHECKPOINT_TIMEOUT, MEDIUM_CHECKPOINT_TEST_HEIGHT, STOP_AT_HEIGHT_REGEX,
        STOP_ON_LOAD_TIMEOUT, SYNC_FINISHED_REGEX, TINY_CHECKPOINT_TEST_HEIGHT,
        TINY_CHECKPOINT_TIMEOUT,
    },
    test_type::TestType::{self, *},
};

use crate::common::regtest::MiningRpcMethods;

/// The maximum amount of time that we allow the creation of a future to block the `tokio` executor.
///
/// This should be larger than the amount of time between thread time slices on a busy test VM.
///
/// This limit only applies to some tests.
pub const MAX_ASYNC_BLOCKING_TIME: Duration = zebra_test::mock_service::DEFAULT_MAX_REQUEST_DELAY;

#[test]
fn generate_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let child = testdir()?
        .with_config(&mut default_test_config(&Mainnet)?)?
        .spawn_child(args!["generate"])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_line_contains("# Default configuration for zebrad")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(args!["generate", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["generate", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let child = testdir.spawn_child(args!["generate", "-o"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    // Valid
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        testdir.path().exists(),
        &output,
        "test temp directory not found"
    );
    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;

    let child = testdir.spawn_child(args!["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The first line should have the version
    output.any_output_line(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "are valid zebrad semantic versions",
    )?;

    // Make sure we are in help by looking for the usage string
    output.stdout_line_contains("Usage:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // The subcommand "argument" wasn't recognized.
    let child = testdir.spawn_child(args!["help", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let child = testdir.spawn_child(args!["help", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_line_contains("Starting zebrad")?;

    // Make sure the command passed the legacy chain check
    output.stdout_line_contains("starting legacy chain check")?;
    output.stdout_line_contains("no legacy chain found")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[tokio::test]
async fn db_init_outside_future_executor() -> Result<()> {
    let _init_guard = zebra_test::init();
    let config = default_test_config(&Mainnet)?;

    let start = Instant::now();

    // This test doesn't need UTXOs to be verified efficiently, because it uses an empty state.
    let db_init_handle = {
        let config = config.clone();
        tokio::spawn(async move {
            zebra_state::init(
                config.state.clone(),
                &config.network.network,
                Height::MAX,
                0,
            )
            .await
        })
    };

    // it's faster to panic if it takes longer than expected, since the executor
    // will wait indefinitely for blocking operation to finish once started
    let block_duration = start.elapsed();
    assert!(
        block_duration <= MAX_ASYNC_BLOCKING_TIME,
        "futures executor was blocked longer than expected ({block_duration:?})",
    );

    db_init_handle.await.map_err(|e| eyre!(e))?;

    Ok(())
}

/// Check that the block state and peer list caches are written to disk.
#[test]
fn persistent_mode() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "state directory empty despite persistent state config"
    );

    let cache_dir = testdir.path().join("network");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "network directory empty despite persistent network config"
    );

    Ok(())
}

#[test]
fn ephemeral_existing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::ExistingDirectory)
}

#[test]
fn ephemeral_missing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::MissingDirectory)
}

#[test]
fn misconfigured_ephemeral_existing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::ExistingDirectory,
    )
}

#[test]
fn misconfigured_ephemeral_missing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::MissingDirectory,
    )
}

/// Check that the state directory created on disk matches the state config.
///
/// TODO: do a similar test for `network.cache_dir`
#[tracing::instrument]
fn ephemeral(cache_dir_config: EphemeralConfig, cache_dir_check: EphemeralCheck) -> Result<()> {
    use std::io::ErrorKind;

    let _init_guard = zebra_test::init();

    let mut config = default_test_config(&Mainnet)?;
    let run_dir = testdir()?;

    let ignored_cache_dir = run_dir.path().join("state");
    if cache_dir_config == EphemeralConfig::MisconfiguredCacheDir {
        // Write a configuration that sets both the cache_dir and ephemeral options
        config.state.cache_dir.clone_from(&ignored_cache_dir);
    }
    if cache_dir_check == EphemeralCheck::ExistingDirectory {
        // We set the cache_dir config to a newly created empty temp directory,
        // then make sure that it is empty after the test
        fs::create_dir(&ignored_cache_dir)?;
    }

    let mut child = run_dir
        .path()
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let (expected_run_dir_file_names, optional_run_dir_file_names) = match cache_dir_check {
        // we created the state directory, so it should still exist
        EphemeralCheck::ExistingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect("ignored_cache_dir should still exist")
                    .count()
                    == 0,
                &output,
                "ignored_cache_dir not empty for ephemeral {:?} {:?}: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );
            (["state", "zebrad.toml"].iter(), ["network"].iter())
        }

        // we didn't create the state directory, so it should not exist
        EphemeralCheck::MissingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect_err("ignored_cache_dir should not exist")
                    .kind()
                    == ErrorKind::NotFound,
                &output,
                "unexpected creation of ignored_cache_dir for ephemeral {:?} {:?}: the cache dir exists and contains these files: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );

            (["zebrad.toml"].iter(), ["network"].iter())
        }
    };

    let expected_run_dir_file_names: HashSet<std::ffi::OsString> =
        expected_run_dir_file_names.map(Into::into).collect();

    let optional_run_dir_file_names: HashSet<std::ffi::OsString> =
        optional_run_dir_file_names.map(Into::into).collect();

    let run_dir_file_names = run_dir
        .path()
        .read_dir()
        .expect("run_dir should still exist")
        .map(|dir_entry| dir_entry.expect("run_dir is readable").file_name())
        // ignore directory list order, because it can vary based on the OS and filesystem
        .collect::<HashSet<_>>();

    let has_expected_file_paths = expected_run_dir_file_names
        .iter()
        .all(|expected_file_name| run_dir_file_names.contains(expected_file_name));

    let has_only_allowed_file_paths = run_dir_file_names.iter().all(|file_name| {
        optional_run_dir_file_names.contains(file_name)
            || expected_run_dir_file_names.contains(file_name)
    });

    assert_with_context!(
        has_expected_file_paths && has_only_allowed_file_paths,
        &output,
        "run_dir not empty for ephemeral {:?} {:?}: expected {:?}, actual: {:?}",
        cache_dir_config,
        cache_dir_check,
        expected_run_dir_file_names,
        run_dir_file_names
    );

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;

    let child = testdir.spawn_child(args!["--version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The output should only contain the version
    output.output_check(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["tip-height", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f` is ignored
    let child = testdir.spawn_child(args!["--version", "-f"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The output should only contain the version
    output.output_check(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

    Ok(())
}

/// Run config tests that use the default ports and paths.
///
/// Unlike the other tests, these tests can not be run in parallel, because
/// they use the generated config. So parallel execution can cause port and
/// cache conflicts.
#[test]
fn config_tests() -> Result<()> {
    valid_generated_config("start", "Starting zebrad")?;

    // Check what happens when Zebra parses an invalid config
    invalid_generated_config()?;

    // Check that we have a current version of the config stored
    #[cfg(not(target_os = "windows"))]
    last_config_is_stored()?;

    // Check that Zebra's previous configurations still work
    stored_configs_work()?;

    // We run the `zebrad` app test after the config tests, to avoid potential port conflicts
    app_no_args()?;

    Ok(())
}

/// Test that `zebrad` runs the start command with no args
#[tracing::instrument]
fn app_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;

    tracing::info!(?testdir, "running zebrad with no config (default settings)");

    let mut child = testdir.spawn_child(args![])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(true)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_line_contains("Starting zebrad")?;

    // Make sure the command passed the legacy chain check
    output.stdout_line_contains("starting legacy chain check")?;
    output.stdout_line_contains("no legacy chain found")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

/// Test that `zebrad start` can parse the output from `zebrad generate`.
#[tracing::instrument]
fn valid_generated_config(command: &str, expect_stdout_line_contains: &str) -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    tracing::info!(?generated_config_path, "generating valid config");

    // Generate configuration in temp dir path
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    tracing::info!(?generated_config_path, "testing valid config parsing");

    // Run command using temp dir and kill it after a few seconds
    let mut child = testdir.spawn_child(args![command])?;
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_line_contains(expect_stdout_line_contains)?;

    // [Note on port conflict](#Note on port conflict)
    output.assert_was_killed().wrap_err("Possible port or cache conflict. Are there other acceptance test, zebrad, or zcashd processes running?")?;

    assert_with_context!(
        testdir.path().exists(),
        &output,
        "test temp directory not found"
    );
    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    Ok(())
}

/// Check if the config produced by current zebrad is stored.
#[cfg(not(target_os = "windows"))]
#[tracing::instrument]
#[allow(clippy::print_stdout)]
fn last_config_is_stored() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    tracing::info!(?generated_config_path, "generated current config");

    // Generate configuration in temp dir path
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    tracing::info!(
        ?generated_config_path,
        "testing current config is in stored configs"
    );

    // Get the contents of the generated config file
    let generated_content =
        fs::read_to_string(generated_config_path).expect("Should have been able to read the file");

    // We need to replace the cache dir path as stored configs has a dummy `cache_dir` string there.
    let processed_generated_content = generated_content
        .replace(
            zebra_state::Config::default()
                .cache_dir
                .to_str()
                .expect("a valid cache dir"),
            "cache_dir",
        )
        .trim()
        .to_string();

    // Loop all the stored configs
    //
    // TODO: use the same filename list code in last_config_is_stored() and stored_configs_work()
    for config_file in configs_dir()
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_string_lossy();

        if config_file_name.as_ref().starts_with('.') || config_file_name.as_ref().starts_with('#')
        {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        // Read stored config
        let stored_content = fs::read_to_string(config_file_full_path(config_file_path))
            .expect("Should have been able to read the file")
            .trim()
            .to_string();

        // If any stored config is equal to the generated then we are good.
        if stored_content.eq(&processed_generated_content) {
            return Ok(());
        }
    }

    println!(
        "\n\
         Here is the missing config file: \n\
         \n\
         {processed_generated_content}\n"
    );

    Err(eyre!(
        "latest zebrad config is not being tested for compatibility. \n\
         \n\
         Take the missing config file logged above, \n\
         and commit it to Zebra's git repository as:\n\
         zebrad/tests/common/configs/<next-release-tag>.toml \n\
         \n\
         Or run: \n\
         cargo build --bin zebrad && \n\
         zebrad generate | \n\
         sed 's/cache_dir = \".*\"/cache_dir = \"cache_dir\"/' > \n\
         zebrad/tests/common/configs/<next-release-tag>.toml",
    ))
}

/// Checks that Zebra prints an informative message when it cannot parse the
/// config file.
#[tracing::instrument]
fn invalid_generated_config() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = &testdir()?;

    // Add a config file name to tempdir path.
    let config_path = testdir.path().join("zebrad.toml");

    tracing::info!(
        ?config_path,
        "testing invalid config parsing: generating valid config"
    );

    // Generate a valid config file in the temp dir.
    let child = testdir.spawn_child(args!["generate", "-o": config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        config_path.exists(),
        &output,
        "generated config file not found"
    );

    // Load the valid config file that Zebra generated.
    let mut config_file = fs::read_to_string(config_path.to_str().unwrap()).unwrap();

    // Let's now alter the config file so that it contains a deprecated format
    // of `mempool.eviction_memory_time`.

    config_file = config_file
        .lines()
        // Remove the valid `eviction_memory_time` key/value pair from the
        // config.
        .filter(|line| !line.contains("eviction_memory_time"))
        .map(|line| line.to_owned() + "\n")
        .collect();

    // Append the `eviction_memory_time` key/value pair in a deprecated format.
    config_file += r"

            [mempool.eviction_memory_time]
            nanos = 0
            secs = 3600
    ";

    tracing::info!(?config_path, "writing invalid config");

    // Write the altered config file so that Zebra can pick it up.
    fs::write(config_path.to_str().unwrap(), config_file.as_bytes())
        .expect("Could not write the altered config file.");

    tracing::info!(?config_path, "testing invalid config parsing");

    // Run Zebra in a temp dir so that it loads the config.
    let mut child = testdir.spawn_child(args!["start"])?;

    // Return an error if Zebra is running for more than two seconds.
    //
    // Since the config is invalid, Zebra should terminate instantly after its
    // start. Two seconds should be sufficient for Zebra to read the config file
    // and terminate.
    std::thread::sleep(Duration::from_secs(2));
    if child.is_running() {
        // We're going to error anyway, so return an error that makes sense to the developer.
        child.kill(true)?;
        return Err(eyre!(
            "Zebra should have exited after reading the invalid config"
        ));
    }

    let output = child.wait_with_output()?;

    // Check that Zebra produced an informative message.
    output.stderr_contains(
        "Zebra could not load the provided configuration file and/or environment variables",
    )?;

    Ok(())
}

/// Test all versions of `zebrad.toml` we have stored can be parsed by the latest `zebrad`.
#[tracing::instrument]
#[test]
fn stored_configs_parsed_correctly() -> Result<()> {
    let old_configs_dir = configs_dir();
    use abscissa_core::Application;
    use zebrad::application::ZebradApp;

    tracing::info!(?old_configs_dir, "testing older config parsing");

    for config_file in old_configs_dir
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_str()
            .expect("config file names are valid unicode");

        if config_file_name.starts_with('.') || config_file_name.starts_with('#') {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        tracing::info!(
            ?config_file_path,
            "testing old config can be parsed by current zebrad"
        );

        ZebradApp::default()
            .load_config(&config_file_path)
            .expect("config should parse");
    }

    Ok(())
}

/// Test all versions of `zebrad.toml` we have stored can be parsed by the latest `zebrad`.
#[tracing::instrument]
fn stored_configs_work() -> Result<()> {
    let old_configs_dir = configs_dir();

    tracing::info!(?old_configs_dir, "testing older config parsing");

    for config_file in old_configs_dir
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_str()
            .expect("config file names are valid unicode");

        if config_file_name.starts_with('.') || config_file_name.starts_with('#') {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        let run_dir = testdir()?;
        let stored_config_path = config_file_full_path(config_file.path());

        tracing::info!(
            ?stored_config_path,
            "testing old config can be parsed by current zebrad"
        );

        // run zebra with stored config
        let mut child =
            run_dir.spawn_child(args!["-c", stored_config_path.to_str().unwrap(), "start"])?;

        let success_regexes = [
            // When logs are sent to the terminal, we see the config loading message and path.
            format!("Using config file at:.*{}", regex::escape(config_file_name)),
            // If they are sent to a file, we see a log file message on stdout,
            // and a logo, welcome message, and progress bar on stderr.
            "Sending logs to".to_string(),
            // TODO: add expect_stdout_or_stderr_line_matches() and check for this instead:
            //"Thank you for running a mainnet zebrad".to_string(),
        ];

        tracing::info!(
            ?stored_config_path,
            ?success_regexes,
            "waiting for zebrad to parse config and start logging"
        );

        let success_regexes = success_regexes
            .iter()
            .collect_regex_set()
            .expect("regexes are valid");

        // Zebra was able to start with the stored config.
        child.expect_stdout_line_matches(success_regexes)?;

        // finish
        child.kill(false)?;

        let output = child.wait_with_output()?;
        let output = output.assert_failure()?;

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }

    Ok(())
}

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint on testnet.
///
/// The first checkpoint contains a single genesis block.
// TODO: disabled because testnet is not currently reliable
// #[test]
#[allow(dead_code)]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        &Network::new_default_testnet(),
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync the first checkpoint, restart, and stop on load.
#[test]
fn restart_stop_at_height() -> Result<()> {
    let _init_guard = zebra_test::init();

    restart_stop_at_height_for_network(Network::Mainnet, TINY_CHECKPOINT_TEST_HEIGHT)?;
    // TODO: disabled because testnet is not currently reliable
    // restart_stop_at_height_for_network(Network::Testnet, TINY_CHECKPOINT_TEST_HEIGHT)?;

    Ok(())
}

fn restart_stop_at_height_for_network(network: Network, height: block::Height) -> Result<()> {
    let reuse_tempdir = sync_until(
        height,
        &network,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )?;
    // if stopping corrupts the rocksdb database, zebrad might hang or crash here
    // if stopping does not write the rocksdb database to disk, Zebra will
    // sync, rather than stopping immediately at the configured height
    sync_until(
        height,
        &network,
        "state is already at the configured height",
        STOP_ON_LOAD_TIMEOUT,
        reuse_tempdir,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        false,
    )?;

    Ok(())
}

/// Test if `zebrad` can activate the mempool on mainnet.
/// Debug activation happens after committing the genesis block.
#[test]
fn activate_mempool_mainnet() -> Result<()> {
    sync_until(
        block::Height(TINY_CHECKPOINT_TEST_HEIGHT.0 + 1),
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ForceActivationAt(TINY_CHECKPOINT_TEST_HEIGHT),
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

/// Test if `zebrad` can sync some larger checkpoints on mainnet.
///
/// This test might fail or timeout on slow or unreliable networks,
/// so we don't run it by default. It also takes a lot longer than
/// our 10 second target time for default tests.
#[test]
#[ignore]
fn sync_large_checkpoints_empty() -> Result<()> {
    // Skip unless explicitly enabled
    if std::env::var("TEST_LARGE_CHECKPOINTS").is_err() {
        tracing::warn!(
            "Skipped sync_large_checkpoints_empty, set the TEST_LARGE_CHECKPOINTS environmental variable to run the test"
        );
        return Ok(());
    }

    let reuse_tempdir = sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )?;
    // if this sync fails, see the failure notes in `restart_stop_at_height`
    sync_until(
        (LARGE_CHECKPOINT_TEST_HEIGHT - 1).unwrap(),
        &Mainnet,
        "previous state height is greater than the stop height",
        STOP_ON_LOAD_TIMEOUT,
        reuse_tempdir,
        MempoolBehavior::ShouldNotActivate,
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        false,
    )?;

    Ok(())
}

// TODO: We had `sync_large_checkpoints_empty` and `sync_large_checkpoints_mempool_testnet`,
// but they were removed because the testnet is unreliable (#1222).
// We should re-add them after we have more testnet instances (#1791).

/// Test if `zebrad` can run side by side with the mempool.
/// This is done by running the mempool and syncing some checkpoints.
#[test]
#[ignore]
fn sync_large_checkpoints_mempool_mainnet() -> Result<()> {
    sync_until(
        MEDIUM_CHECKPOINT_TEST_HEIGHT,
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ForceActivationAt(TINY_CHECKPOINT_TEST_HEIGHT),
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}

#[tracing::instrument]
fn create_cached_database(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height();
    let checkpoint_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit checkpoint-verified request");

    create_cached_database_height(
        &network,
        height,
        // Use checkpoints to increase sync performance while caching the database
        true,
        // Check that we're still using checkpoints when we finish the cached sync
        &checkpoint_stop_regex,
    )
}

#[tracing::instrument]
fn sync_past_mandatory_checkpoint(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height() + (32_257 + 1200);
    let full_validation_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit contextually-verified request");

    create_cached_database_height(
        &network,
        height.unwrap(),
        // Test full validation by turning checkpoints off
        false,
        // Check that we're doing full validation when we finish the cached sync
        &full_validation_stop_regex,
    )
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
#[allow(clippy::print_stderr)]
#[tracing::instrument]
fn full_sync_test(network: Network, timeout_argument_name: &str) -> Result<()> {
    let timeout_argument: Option<u64> = env::var(timeout_argument_name)
        .ok()
        .and_then(|timeout_string| timeout_string.parse().ok());

    // # TODO
    //
    // Replace hard-coded values in create_cached_database_height with:
    // - the timeout in the environmental variable
    // - the path from the resolved config (state.cache_dir)
    if let Some(_timeout_minutes) = timeout_argument {
        create_cached_database_height(
            &network,
            // Just keep going until we reach the chain tip
            block::Height::MAX,
            // Use the checkpoints to sync quickly, then do full validation until the chain tip
            true,
            // Finish when we reach the chain tip
            SYNC_FINISHED_REGEX,
        )
    } else {
        tracing::warn!(
            "Skipped full sync test for {network}, \
            set the {timeout_argument_name:?} environmental variable to run the test",
        );

        Ok(())
    }
}

// These tests are ignored because they're too long running to run during our
// traditional CI, and they depend on persistent state that cannot be made
// available in github actions or google cloud build. Instead we run these tests
// directly in a vm we spin up on google compute engine, where we can mount
// drives populated by the sync_to_mandatory_checkpoint tests, snapshot those drives,
// and then use them to more quickly run the sync_past_mandatory_checkpoint tests.

/// Sync up to the mandatory checkpoint height on mainnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_mainnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Mainnet)
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_testnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Network::new_default_testnet())
}

/// Helper function for sync to checkpoint tests
fn sync_to_mandatory_checkpoint_for_network(network: Network) -> Result<()> {
    use std::env;

    // Skip unless explicitly enabled
    if env::var("TEST_SYNC_TO_CHECKPOINT").is_err() {
        return Ok(());
    }

    let _init_guard = zebra_test::init();
    create_cached_database(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_mainnet() -> Result<()> {
    // Skip unless explicitly enabled
    if std::env::var("TEST_SYNC_PAST_CHECKPOINT").is_err() {
        tracing::warn!(
            "Skipped sync_past_mandatory_checkpoint_mainnet, set the TEST_SYNC_PAST_CHECKPOINT environmental variable to run the test"
        );
        return Ok(());
    }
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    sync_past_mandatory_checkpoint(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on testnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_testnet() -> Result<()> {
    // Skip unless explicitly enabled
    if std::env::var("TEST_SYNC_PAST_CHECKPOINT").is_err() {
        tracing::warn!(
            "Skipped sync_past_mandatory_checkpoint_testnet, set the TEST_SYNC_PAST_CHECKPOINT environmental variable to run the test"
        );
        return Ok(());
    }
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    sync_past_mandatory_checkpoint(network)
}

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_mainnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "SYNC_FULL_MAINNET_TIMEOUT_MINUTES")
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_testnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(
        Network::new_default_testnet(),
        "SYNC_FULL_TESTNET_TIMEOUT_MINUTES",
    )
}

#[cfg(all(feature = "prometheus", not(target_os = "windows")))]
#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::io::Write;

    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url = format!("http://{endpoint}");

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config(&Mainnet)?;
    config.metrics.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let child = dir.spawn_child(args!["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    // Test metrics endpoint
    let res = client.get(url.try_into().expect("url is valid")).await;

    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let mut body = Vec::new();
    let mut body_stream = res.into_body();
    while let Some(next) = body_stream.frame().await {
        body.write_all(next?.data_ref().unwrap())?;
    }

    let (body, mut child) = child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(body))?;
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.any_output_line_contains(
        "# TYPE zebrad_build_info counter",
        &body,
        "metrics exporter response",
        "the metrics response header",
    )?;
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in metrics exporter response");

    // Make sure metrics was started
    output.stdout_line_contains(format!("Opened metrics endpoint at {endpoint}").as_str())?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

#[cfg(all(feature = "filter-reload", not(target_os = "windows")))]
#[tokio::test]
async fn tracing_endpoint() -> Result<()> {
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::io::Write;

    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url_default = format!("http://{endpoint}");
    let url_filter = format!("{url_default}/filter");

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config(&Mainnet)?;
    config.tracing.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let child = dir.spawn_child(args!["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    // Test tracing endpoint
    let res = client
        .get(url_default.try_into().expect("url_default is valid"))
        .await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let mut body = Vec::new();
    let mut body_stream = res.into_body();
    while let Some(next) = body_stream.frame().await {
        body.write_all(next?.data_ref().unwrap())?;
    }

    let (body, child) = child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(body))?;

    // Set a filter and make sure it was changed
    let request = hyper::Request::post(url_filter.clone())
        .body("zebrad=debug".to_string().into())
        .unwrap();

    let post = client.request(request).await;
    let (_post, child) = child.kill_on_error(post)?;

    let tracing_res = client
        .get(url_filter.try_into().expect("url_filter is valid"))
        .await;

    let (tracing_res, child) = child.kill_on_error(tracing_res)?;
    assert!(tracing_res.status().is_success());

    // Get the body of the response
    let mut tracing_body = Vec::new();
    let mut body_stream = tracing_res.into_body();
    while let Some(next) = body_stream.frame().await {
        tracing_body.write_all(next?.data_ref().unwrap())?;
    }

    let (tracing_body, mut child) =
        child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(tracing_body.clone()))?;

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_line_contains(format!("Opened tracing endpoint at {endpoint}").as_str())?;
    // TODO: Match some trace level messages from output

    // Make sure the endpoint header is correct
    // The header is split over two lines. But we don't want to require line
    // breaks at a specific word, so we run two checks for different substrings.

    output.any_output_line_contains(
        "HTTP endpoint allows dynamic control of the filter",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    output.any_output_line_contains(
        "tracing events",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in tracing filter response");

    // Make sure endpoint requests change the filter
    output.any_output_line_contains(
        "zebrad=debug",
        &tracing_body,
        "tracing filter endpoint response",
        "the modified tracing filter",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in modified tracing filter response");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with a single thread.
#[tokio::test]
async fn rpc_endpoint_single_thread() -> Result<()> {
    rpc_endpoint(false).await
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with multiple threads.
#[tokio::test]
async fn rpc_endpoint_parallel_threads() -> Result<()> {
    rpc_endpoint(true).await
}

/// Test that the JSON-RPC endpoint responds to a request.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
#[tracing::instrument]
async fn rpc_endpoint(parallel_cpu_threads: bool) -> Result<()> {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = os_assigned_rpc_port_config(parallel_cpu_threads, &Mainnet)?;

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Run `zebrad` for a few seconds before testing the endpoint
    std::thread::sleep(LAUNCH_DELAY);

    // Make the call to the `getinfo` RPC method
    let res = client.call("getinfo", "[]".to_string()).await?;

    // Test rpc endpoint response
    assert!(res.status().is_success());

    let body = res.bytes().await;
    let (body, mut child) = child.kill_on_error(body)?;

    let parsed: Value = serde_json::from_slice(&body)?;

    // Check that we have at least 4 characters in the `build` field.
    let build = parsed["result"]["build"].as_str().unwrap();
    assert!(build.len() > 4, "Got {build}");

    // Check that the `subversion` field has "Zebra" in it.
    let subversion = parsed["result"]["subversion"].as_str().unwrap();
    assert!(subversion.contains("Zebra"), "Got {subversion}");

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to requests with different content types.
///
/// This test ensures that the curl examples of zcashd rpc methods will also work in Zebra.
///
/// https://zcash.github.io/rpc/getblockchaininfo.html
#[tokio::test]
async fn rpc_endpoint_client_content_type() -> Result<()> {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = random_known_rpc_port_config(true, &Mainnet)?;

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Call to `getinfo` RPC method with a no content type.
    let res = client
        .call_with_no_content_type("getinfo", "[]".to_string())
        .await?;

    // Zebra will insert valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain`.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain` content type as the zcashd rpc docs.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain;".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain; other string` content type.
    let res = client
        .call_with_content_type(
            "getinfo",
            "[]".to_string(),
            "text/plain; other string".to_string(),
        )
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a valid `application/json` content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "application/json".to_string())
        .await?;

    // Zebra will not replace valid content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with invalid string as content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "whatever".to_string())
        .await?;

    // Zebra will not replace unrecognized content type and fail.
    assert!(res.status().is_client_error());

    Ok(())
}

/// Test that Zebra's non-blocking logger works, by creating lots of debug output, but not reading the logs.
/// Then make sure Zebra drops excess log lines. (Previously, it would block waiting for logs to be read.)
///
/// This test is unreliable and sometimes hangs on macOS.
#[test]
#[cfg(not(target_os = "macos"))]
fn non_blocking_logger() -> Result<()> {
    use futures::FutureExt;
    use std::{sync::mpsc, time::Duration};

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (done_tx, done_rx) = mpsc::channel();

    let test_task_handle: tokio::task::JoinHandle<Result<()>> = rt.spawn(async move {
        let mut config = os_assigned_rpc_port_config(false, &Mainnet)?;
        config.tracing.filter = Some("trace".to_string());
        config.tracing.buffer_limit = 100;

        let dir = testdir()?.with_config(&mut config)?;
        let mut child = dir
            .spawn_child(args!["start"])?
            .with_timeout(TINY_CHECKPOINT_TIMEOUT);

        // Wait until port is open.
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

        // Create an http client
        let client = RpcRequestClient::new(rpc_address);

        // Most of Zebra's lines are 100-200 characters long, so 500 requests should print enough to fill the unix pipe,
        // fill the channel that tracing logs are queued onto, and drop logs rather than block execution.
        for _ in 0..500 {
            let res = client.call("getinfo", "[]".to_string()).await?;

            // Test that zebrad rpc endpoint is still responding to requests
            assert!(res.status().is_success());
        }

        child.kill(false)?;

        let output = child.wait_with_output()?;
        let output = output.assert_failure()?;

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

        done_tx.send(())?;

        Ok(())
    });

    // Wait until the spawned task finishes up to 45 seconds before shutting down tokio runtime
    if done_rx.recv_timeout(Duration::from_secs(90)).is_ok() {
        rt.shutdown_timeout(Duration::from_secs(3));
    }

    match test_task_handle.now_or_never() {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(eyre!("join error: {:?}", error)),
        None => Err(eyre!("unexpected test task hang")),
    }
}

/// Make sure `lightwalletd` works with Zebra, when both their states are empty.
///
/// This test only runs when the `TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lwd_integration() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })
}

/// Make sure `zebrad` can sync from peers, but don't actually launch `lightwalletd`.
///
/// This test only runs when a persistent cached state directory path is configured
/// (for example, by setting `ZEBRA_STATE__CACHE_DIR`).
///
/// This test might work on Windows.
#[test]
#[ignore]
fn sync_update_mainnet() -> Result<()> {
    lwd_integration_test(UpdateZebraCachedStateNoRpc)
}

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state directory path is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lwd_sync_update() -> Result<()> {
    lwd_integration_test(UpdateCachedState)
}

/// Make sure `lightwalletd` can fully sync from genesis using Zebra.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lwd_sync_full() -> Result<()> {
    lwd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    })
}

/// Make sure `lightwalletd` can sync from Zebra, in all available modes.
///
/// Runs the tests in this order:
/// - launch lightwalletd with empty states,
/// - if a cached Zebra state directory path is configured:
///   - run a full sync
/// - if a cached Zebra state directory path is configured:
///   - run a quick update sync,
///   - run a send transaction gRPC test,
///   - run read-only gRPC tests.
///
/// The lightwalletd full, update, and gRPC tests only run with `--features=lightwalletd-grpc-tests`.
///
/// These tests don't work on Windows, so they are always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(not(target_os = "windows"))]
async fn lightwalletd_test_suite() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })?;

    // Only runs when a cached Zebra state directory path is configured with an environment variable.
    lwd_integration_test(UpdateZebraCachedStateNoRpc)?;

    // These tests need the compile-time gRPC feature
    #[cfg(feature = "lightwalletd-grpc-tests")]
    {
        // Do the quick tests first

        // Only runs when a cached Zebra state is configured
        lwd_integration_test(UpdateCachedState)?;

        // Only runs when a cached Zebra state is configured
        common::lightwalletd::wallet_grpc_test::run().await?;

        // Then do the slow tests

        // Only runs when a cached Zebra state is configured.
        // When manually running the test suite, allow cached state in the full sync test.
        lwd_integration_test(FullSyncFromGenesis {
            allow_lightwalletd_cached_state: true,
        })?;

        // Only runs when a cached Zebra state is configured
        common::lightwalletd::send_transaction_test::run().await?;
    }

    Ok(())
}

/// Run a lightwalletd integration test with a configuration for `test_type`.
///
/// Tests that sync `lightwalletd` to the chain tip require the `lightwalletd-grpc-tests` feature`:
/// - [`FullSyncFromGenesis`]
/// - [`UpdateCachedState`]
///
/// Set `FullSyncFromGenesis { allow_lightwalletd_cached_state: true }` to speed up manual full sync tests.
///
/// # Relibility
///
/// The random ports in this test can cause [rare port conflicts.](#Note on port conflict)
///
/// # Panics
///
/// If the `test_type` requires `--features=lightwalletd-grpc-tests`,
/// but Zebra was not compiled with that feature.
#[tracing::instrument]
fn lwd_integration_test(test_type: TestType) -> Result<()> {
    let _init_guard = zebra_test::init();

    // We run these sync tests with a network connection, for better test coverage.
    let use_internet_connection = true;
    let network = Mainnet;
    let test_name = "lwd_integration_test";

    if test_type.launches_lightwalletd() && !can_spawn_lightwalletd_for_rpc(test_name, test_type) {
        tracing::info!("skipping test due to missing lightwalletd network or cached state");
        return Ok(());
    }

    // Launch zebra with peers and using a predefined zebrad state path.
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network.clone(),
        test_name,
        test_type,
        use_internet_connection,
    )? {
        tracing::info!(
            ?test_type,
            "running lightwalletd & zebrad integration test, launching zebrad...",
        );

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    if test_type.needs_zebra_cached_state() {
        zebrad
            .expect_stdout_line_matches(r"loaded Zebra state cache .*tip.*=.*Height\([0-9]{7}\)")?;
    } else {
        // Timeout the test if we're somehow accidentally using a cached state
        zebrad.expect_stdout_line_matches("loaded Zebra state cache .*tip.*=.*None")?;
    }

    // Wait for the state to upgrade and the RPC port, if the upgrade is short.
    //
    // If incompletely upgraded states get written to the CI cache,
    // change DATABASE_FORMAT_UPGRADE_IS_LONG to true.
    if !DATABASE_FORMAT_UPGRADE_IS_LONG {
        if test_type.launches_lightwalletd() {
            tracing::info!(
                ?test_type,
                ?zebra_rpc_address,
                "waiting for zebrad to open its RPC port..."
            );
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                [format!(
                    "Opened RPC endpoint at {}",
                    zebra_rpc_address.expect("lightwalletd test must have RPC port")
                )],
            )?;
        } else {
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                None,
            )?;
        }
    }

    // Wait for zebrad to sync the genesis block before launching lightwalletd,
    // if lightwalletd is launched and zebrad starts with an empty state.
    // This prevents lightwalletd from exiting early due to an empty state.
    if test_type.launches_lightwalletd() && !test_type.needs_zebra_cached_state() {
        tracing::info!(
            ?test_type,
            "waiting for zebrad to sync genesis block before launching lightwalletd...",
        );
        // Wait for zebrad to commit the genesis block to the state.
        // Use the syncer's state tip log message, as the specific commit log might not appear reliably.
        zebrad.expect_stdout_line_matches(
            "starting sync, obtaining new tips state_tip=Some\\(Height\\(0\\)\\)",
        )?;
    }

    // Launch lightwalletd, if needed
    let lightwalletd_and_port = if test_type.launches_lightwalletd() {
        tracing::info!(
            ?zebra_rpc_address,
            "launching lightwalletd connected to zebrad",
        );

        // Launch lightwalletd
        let (mut lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_for_rpc(
            network,
            test_name,
            test_type,
            zebra_rpc_address.expect("lightwalletd test must have RPC port"),
        )?
        .expect("already checked for lightwalletd cached state and network");

        tracing::info!(
            ?lightwalletd_rpc_port,
            "spawned lightwalletd connected to zebrad",
        );

        // Check that `lightwalletd` is calling the expected Zebra RPCs

        // getblockchaininfo
        if test_type.needs_zebra_cached_state() {
            lightwalletd.expect_stdout_line_matches(
                "Got sapling height 419200 block height [0-9]{7} chain main branchID [0-9a-f]{8}",
            )?;
        } else {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches(
                "Got sapling height 419200 block height [0-9]{1,6} chain main branchID 00000000",
            )?;
        }

        if test_type.needs_lightwalletd_cached_state() {
            lightwalletd
                .expect_stdout_line_matches("Done reading [0-9]{7} blocks from disk cache")?;
        } else if !test_type.allow_lightwalletd_cached_state() {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches("Done reading 0 blocks from disk cache")?;
        }

        // getblock with the first Sapling block in Zebra's state
        //
        // zcash/lightwalletd calls getbestblockhash here, but
        // adityapk00/lightwalletd calls getblock
        //
        // The log also depends on what is in Zebra's state:
        //
        // # Cached Zebra State
        //
        // lightwalletd ingests blocks into its cache.
        //
        // # Empty Zebra State
        //
        // lightwalletd tries to download the Sapling activation block, but it's not in the state.
        //
        // Until the Sapling activation block has been downloaded,
        // lightwalletd will keep retrying getblock.
        if !test_type.allow_lightwalletd_cached_state() {
            if test_type.needs_zebra_cached_state() {
                lightwalletd.expect_stdout_line_matches(
                    "([Aa]dding block to cache)|([Ww]aiting for block)",
                )?;
            } else {
                lightwalletd.expect_stdout_line_matches(regex::escape(
                    "Waiting for zcashd height to reach Sapling activation height (419200)",
                ))?;
            }
        }

        Some((lightwalletd, lightwalletd_rpc_port))
    } else {
        None
    };

    // Wait for zebrad and lightwalletd to sync, if needed.
    let (mut zebrad, lightwalletd) = if test_type.needs_zebra_cached_state() {
        if let Some((lightwalletd, lightwalletd_rpc_port)) = lightwalletd_and_port {
            #[cfg(feature = "lightwalletd-grpc-tests")]
            {
                use common::lightwalletd::sync::wait_for_zebrad_and_lightwalletd_sync;

                tracing::info!(
                    ?lightwalletd_rpc_port,
                    "waiting for zebrad and lightwalletd to sync...",
                );

                let (lightwalletd, mut zebrad) = wait_for_zebrad_and_lightwalletd_sync(
                    lightwalletd,
                    lightwalletd_rpc_port,
                    zebrad,
                    zebra_rpc_address.expect("lightwalletd test must have RPC port"),
                    test_type,
                    // We want to wait for the mempool and network for better coverage
                    true,
                    use_internet_connection,
                )?;

                // Wait for the state to upgrade, if the upgrade is long.
                // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false,
                // or combine "wait for sync" with "wait for state version upgrade".
                if DATABASE_FORMAT_UPGRADE_IS_LONG {
                    wait_for_state_version_upgrade(
                        &mut zebrad,
                        &state_version_message,
                        state_database_format_version_in_code(),
                        None,
                    )?;
                }

                (zebrad, Some(lightwalletd))
            }

            #[cfg(not(feature = "lightwalletd-grpc-tests"))]
            panic!(
                "the {test_type:?} test requires `cargo test --feature lightwalletd-grpc-tests`\n\
                 zebrad: {zebrad:?}\n\
                 lightwalletd: {lightwalletd:?}\n\
                 lightwalletd_rpc_port: {lightwalletd_rpc_port:?}"
            );
        } else {
            // We're just syncing Zebra, so there's no lightwalletd to check
            tracing::info!(?test_type, "waiting for zebrad to sync to the tip");
            zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

            // Wait for the state to upgrade, if the upgrade is long.
            // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false.
            if DATABASE_FORMAT_UPGRADE_IS_LONG {
                wait_for_state_version_upgrade(
                    &mut zebrad,
                    &state_version_message,
                    state_database_format_version_in_code(),
                    None,
                )?;
            }

            (zebrad, None)
        }
    } else {
        let lightwalletd = lightwalletd_and_port.map(|(lightwalletd, _port)| lightwalletd);

        // We don't have a cached state, so we don't do any tip checks for Zebra or lightwalletd
        (zebrad, lightwalletd)
    };

    tracing::info!(
        ?test_type,
        "cleaning up child processes and checking for errors",
    );

    // Cleanup both processes
    //
    // If the test fails here, see the [note on port conflict](#Note on port conflict)
    //
    // zcash/lightwalletd exits by itself, but
    // adityapk00/lightwalletd keeps on going, so it gets killed by the test harness.
    zebrad.kill(false)?;

    if let Some(mut lightwalletd) = lightwalletd {
        lightwalletd.kill(false)?;

        let lightwalletd_output = lightwalletd.wait_with_output()?.assert_failure()?;

        lightwalletd_output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }

    let zebrad_output = zebrad.wait_with_output()?.assert_failure()?;

    zebrad_output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test will start 2 zebrad nodes one after the other using the same Zcash listener.
/// It is expected that the first node spawned will get exclusive use of the port.
/// The second node will panic with the Zcash listener conflict hint added in #1535.
#[test]
#[cfg(not(target_os = "windows"))]
fn zebra_zcash_listener_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created network listen_addr
    let mut config = default_test_config(&Mainnet)?;
    config.network.listen_addr = listen_addr.parse().unwrap();
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!("Opened Zcash protocol endpoint at {listen_addr}"));

    // From another folder create a configuration with the same listener.
    // `network.listen_addr` will be the same in the 2 nodes.
    // (But since the config is ephemeral, they will have different state paths.)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same metrics listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash metrics
/// conflict hint added in #1535.
#[test]
#[cfg(all(feature = "prometheus", not(target_os = "windows")))]
fn zebra_metrics_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created metrics endpoint_addr
    let mut config = default_test_config(&Mainnet)?;
    config.metrics.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened metrics endpoint at {listen_addr}"));

    // From another folder create a configuration with the same endpoint.
    // `metrics.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same tracing listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash tracing
/// conflict hint added in #1535.
#[test]
#[cfg(all(feature = "filter-reload", not(target_os = "windows")))]
fn zebra_tracing_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created tracing endpoint_addr
    let mut config = default_test_config(&Mainnet)?;
    config.tracing.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened tracing endpoint at {listen_addr}"));

    // From another folder create a configuration with the same endpoint.
    // `tracing.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same RPC listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic.
///
/// This test is sometimes unreliable on Windows, and hangs on macOS.
/// We believe this is a CI infrastructure issue, not a platform-specific issue.
#[test]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn zebra_rpc_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    //
    // This is the required setting to detect port conflicts.
    let mut config = random_known_rpc_port_config(false, &Mainnet)?;

    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(
        r"Opened RPC endpoint at {}",
        config.rpc.listen_addr.unwrap(),
    ));

    // From another folder create a configuration with the same endpoint.
    // `rpc.listen_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, "Address already in use")?;

    Ok(())
}

/// Start 2 zebrad nodes using the same state directory, but different Zcash
/// listener ports. The first node should get exclusive access to the database.
/// The second node will panic with the Zcash state conflict hint added in #1535.
#[test]
fn zebra_state_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // A persistent config has a fixed temp state directory, but asks the OS to
    // automatically choose an unused port
    let mut config = persistent_test_config(&Mainnet)?;
    let dir_conflict = testdir()?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let contains = if cfg!(unix) {
        let mut dir_conflict_full = PathBuf::new();
        dir_conflict_full.push(dir_conflict.path());
        dir_conflict_full.push("state");
        dir_conflict_full.push(format!(
            "v{}",
            zebra_state::state_database_format_version_in_code().major,
        ));
        dir_conflict_full.push(config.network.network.to_string().to_lowercase());
        format!(
            "Opened Zebra state cache at {}",
            dir_conflict_full.display()
        )
    } else {
        String::from("Opened Zebra state cache at ")
    };

    check_config_conflict(
        dir_conflict.path(),
        regex::escape(&contains).as_str(),
        dir_conflict.path(),
        LOCK_FILE_ERROR.as_str(),
    )?;

    Ok(())
}

/// Launch a node in `first_dir`, wait a few seconds, then launch a node in
/// `second_dir`. Check that the first node's stdout contains
/// `first_stdout_regex`, and the second node's stderr contains
/// `second_stderr_regex`.
#[tracing::instrument]
fn check_config_conflict<T, U>(
    first_dir: T,
    first_stdout_regex: &str,
    second_dir: U,
    second_stderr_regex: &str,
) -> Result<()>
where
    T: ZebradTestDirExt + std::fmt::Debug,
    U: ZebradTestDirExt + std::fmt::Debug,
{
    // Start the first node
    let mut node1 = first_dir.spawn_child(args!["start"])?;

    // Wait until node1 has used the conflicting resource.
    node1.expect_stdout_line_matches(first_stdout_regex)?;

    // Wait a bit before launching the second node.
    std::thread::sleep(BETWEEN_NODES_DELAY);

    // Spawn the second node
    let node2 = second_dir.spawn_child(args!["start"]);
    let (node2, mut node1) = node1.kill_on_error(node2)?;

    // Wait a few seconds and kill first node.
    // Second node is terminated by panic, no need to kill.
    std::thread::sleep(LAUNCH_DELAY);
    let node1_kill_res = node1.kill(false);
    let (_, mut node2) = node2.kill_on_error(node1_kill_res)?;

    // node2 should have panicked due to a conflict. Kill it here anyway, so it
    // doesn't outlive the test on error.
    //
    // This code doesn't work on Windows or macOS. It's cleanup code that only
    // runs when node2 doesn't panic as expected. So it's ok to skip it.
    // See #1781.
    #[cfg(target_os = "linux")]
    if node2.is_running() {
        return node2
            .kill_on_error::<(), _>(Err(eyre!(
                "conflicted node2 was still running, but the test expected a panic"
            )))
            .context_from(&mut node1)
            .map(|_| ());
    }

    // Now we're sure both nodes are dead, and we have both their outputs
    let output1 = node1.wait_with_output().context_from(&mut node2)?;
    let output2 = node2.wait_with_output().context_from(&output1)?;

    // Make sure the first node was killed, rather than exiting with an error.
    output1
        .assert_was_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output2)?;

    // Make sure node2 has the expected resource conflict.
    output2
        .stderr_line_matches(second_stderr_regex)
        .context_from(&output1)?;
    output2
        .assert_was_not_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output1)?;

    Ok(())
}

#[tokio::test]
#[ignore]
async fn lwd_rpc_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateCachedState;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, "lwd_rpc_test", test_type, false)?
    {
        tracing::info!("running fully synced zebrad RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("lightwalletd test must have RPC port");

    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    let client = RpcRequestClient::new(zebra_rpc_address);

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let res = client
        .text_from_call("getblock", r#"["1180900", 0]"#.to_string())
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    let expected_bytes = zebra_test::vectors::MAINNET_BLOCKS
        .get(&1_180_900)
        .expect("test block must exist");
    let expected_hex = hex::encode(expected_bytes);
    assert!(
        res.contains(&expected_hex),
        "response did not contain the desired block: {res}"
    );

    Ok(())
}

#[test]
#[cfg(not(target_os = "windows"))]
fn delete_old_databases() -> Result<()> {
    use std::fs::{canonicalize, create_dir};

    let _init_guard = zebra_test::init();

    // Skip this test because it can be very slow without a network.
    //
    // The delete databases task is launched last during startup, after network setup.
    // If there is no network, network setup can take a long time to timeout,
    // so the task takes a long time to launch, slowing down this test.
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    let mut config = default_test_config(&Mainnet)?;
    let run_dir = testdir()?;
    let cache_dir = run_dir.path().join("state");

    // create cache dir
    create_dir(cache_dir.clone())?;

    // create a v1 dir outside cache dir that should not be deleted
    let outside_dir = run_dir.path().join("v1");
    create_dir(&outside_dir)?;
    assert!(outside_dir.as_path().exists());

    // create a `v1` dir inside cache dir that should be deleted
    let inside_dir = cache_dir.join("v1");
    create_dir(&inside_dir)?;
    let canonicalized_inside_dir = canonicalize(inside_dir.clone()).ok().unwrap();
    assert!(inside_dir.as_path().exists());

    // modify config with our cache dir and not ephemeral configuration
    // (delete old databases function will not run when ephemeral = true)
    config.state.cache_dir = cache_dir;
    config.state.ephemeral = false;

    // run zebra with our config
    let mut child = run_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    // delete checker running
    child.expect_stdout_line_matches("checking for old database versions".to_string())?;

    // inside dir was deleted
    child.expect_stdout_line_matches(format!(
        "deleted outdated state database directory.*deleted_db.*=.*{canonicalized_inside_dir:?}"
    ))?;
    assert!(!inside_dir.as_path().exists());

    // deleting old databases task ended
    child.expect_stdout_line_matches("finished old database version cleanup task".to_string())?;

    // outside dir was not deleted
    assert!(outside_dir.as_path().exists());

    // finish
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test sending transactions using a lightwalletd instance connected to a zebrad instance.
///
/// See [`common::lightwalletd::send_transaction_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
#[cfg(not(target_os = "windows"))]
async fn lwd_rpc_send_tx() -> Result<()> {
    common::lightwalletd::send_transaction_test::run().await
}

/// Test all the rpc methods a wallet connected to lightwalletd can call.
///
/// See [`common::lightwalletd::wallet_grpc_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
#[cfg(not(target_os = "windows"))]
async fn lwd_grpc_wallet() -> Result<()> {
    common::lightwalletd::wallet_grpc_test::run().await
}

/// Test successful getpeerinfo rpc call
///
/// See [`common::get_block_template_rpcs::get_peer_info`] for more information.
#[tokio::test]
async fn get_peer_info() -> Result<()> {
    common::get_block_template_rpcs::get_peer_info::run().await
}

/// Test successful getblocktemplate rpc call
///
/// See [`common::get_block_template_rpcs::get_block_template`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_get_block_template() -> Result<()> {
    common::get_block_template_rpcs::get_block_template::run().await
}

/// Test successful submitblock rpc call
///
/// See [`common::get_block_template_rpcs::submit_block`] for more information.
#[tokio::test]
#[ignore]
async fn rpc_submit_block() -> Result<()> {
    common::get_block_template_rpcs::submit_block::run().await
}

/// Check that the end of support code is called at least once.
#[test]
fn end_of_support_is_checked_at_start() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;
    let mut child = testdir.spawn_child(args!["start"])?;

    // Give enough time to start up the eos task.
    std::thread::sleep(Duration::from_secs(30));

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Zebra started
    output.stdout_line_contains("Starting zebrad")?;

    // End of support task started.
    output.stdout_line_contains("Starting end of support task")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

/// Test `zebra-checkpoints` on mainnet.
///
/// If you want to run this test individually, see the module documentation.
/// See [`common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "zebra-checkpoints")]
async fn generate_checkpoints_mainnet() -> Result<()> {
    common::checkpoints::run(Mainnet).await
}

/// Test `zebra-checkpoints` on testnet.
/// This test might fail if testnet is unstable.
///
/// If you want to run this test individually, see the module documentation.
/// See [`common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "zebra-checkpoints")]
async fn generate_checkpoints_testnet() -> Result<()> {
    common::checkpoints::run(Network::new_default_testnet()).await
}

/// Check that new states are created with the current state format version,
/// and that restarting `zebrad` doesn't change the format version.
#[tokio::test]
async fn new_state_format() -> Result<()> {
    for network in Network::iter() {
        state_format_test("new_state_format_test", &network, 2, None).await?;
    }

    Ok(())
}

/// Check that outdated states are updated to the current state format version,
/// and that restarting `zebrad` doesn't change the updated format version.
///
/// TODO: test partial updates, once we have some updates that take a while.
///       (or just add a delay during tests)
#[tokio::test]
async fn update_state_format() -> Result<()> {
    let mut fake_version = state_database_format_version_in_code();
    fake_version.minor = 0;
    fake_version.patch = 0;

    for network in Network::iter() {
        state_format_test("update_state_format_test", &network, 3, Some(&fake_version)).await?;
    }

    Ok(())
}

/// Check that newer state formats are downgraded to the current state format version,
/// and that restarting `zebrad` doesn't change the format version.
///
/// Future version compatibility is a best-effort attempt, this test can be disabled if it fails.
#[tokio::test]
async fn downgrade_state_format() -> Result<()> {
    let mut fake_version = state_database_format_version_in_code();
    fake_version.minor = u16::MAX.into();
    fake_version.patch = 0;

    for network in Network::iter() {
        state_format_test(
            "downgrade_state_format_test",
            &network,
            3,
            Some(&fake_version),
        )
        .await?;
    }

    Ok(())
}

/// Test state format changes, see calling tests for details.
async fn state_format_test(
    base_test_name: &str,
    network: &Network,
    reopen_count: usize,
    fake_version: Option<&Version>,
) -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_name = &format!("{base_test_name}/new");

    // # Create a new state and check it has the current version

    let zebrad = spawn_zebrad_without_rpc(network.clone(), test_name, false, false, None, false)?;

    // Skip the test unless it has the required state and environmental variables.
    let Some(mut zebrad) = zebrad else {
        return Ok(());
    };

    tracing::info!(?network, "running {test_name} using zebrad");

    zebrad.expect_stdout_line_matches("creating new database with the current format")?;
    zebrad.expect_stdout_line_matches("loaded Zebra state cache")?;

    // Give Zebra enough time to actually write the database to disk.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let logs = zebrad.kill_and_return_output(false)?;

    assert!(
        !logs.contains("marked database format as upgraded"),
        "unexpected format upgrade in logs:\n\
         {logs}"
    );
    assert!(
        !logs.contains("marked database format as downgraded"),
        "unexpected format downgrade in logs:\n\
         {logs}"
    );

    let output = zebrad.wait_with_output()?;
    let mut output = output.assert_failure()?;

    let mut dir = output
        .take_dir()
        .expect("dir should not already have been taken");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    // # Apply the fake version if needed
    let mut expect_older_version = false;
    let mut expect_newer_version = false;

    if let Some(fake_version) = fake_version {
        let test_name = &format!("{base_test_name}/apply_fake_version/{fake_version}");
        tracing::info!(?network, "running {test_name} using zebra-state");

        let config = UseAnyState
            .zebrad_config(test_name, false, Some(dir.path()), network)
            .expect("already checked config")?;

        zebra_state::write_state_database_format_version_to_disk(
            &config.state,
            fake_version,
            network,
        )
        .expect("can't write fake database version to disk");

        // Give zebra_state enough time to actually write the database version to disk.
        tokio::time::sleep(Duration::from_secs(1)).await;

        let running_version = state_database_format_version_in_code();

        match fake_version.cmp(&running_version) {
            Ordering::Less => expect_older_version = true,
            Ordering::Equal => {}
            Ordering::Greater => expect_newer_version = true,
        }
    }

    // # Reopen that state and check the version hasn't changed

    for reopened in 0..reopen_count {
        let test_name = &format!("{base_test_name}/reopen/{reopened}");

        if reopened > 0 {
            expect_older_version = false;
            expect_newer_version = false;
        }

        let mut zebrad =
            spawn_zebrad_without_rpc(network.clone(), test_name, false, false, dir, false)?
                .expect("unexpectedly missing required state or env vars");

        tracing::info!(?network, "running {test_name} using zebrad");

        if expect_older_version {
            zebrad.expect_stdout_line_matches("trying to open older database format")?;
            zebrad.expect_stdout_line_matches("marked database format as upgraded")?;
            zebrad.expect_stdout_line_matches("database is fully upgraded")?;
        } else if expect_newer_version {
            zebrad.expect_stdout_line_matches("trying to open newer database format")?;
            zebrad.expect_stdout_line_matches("marked database format as downgraded")?;
        } else {
            zebrad.expect_stdout_line_matches("trying to open current database format")?;
            zebrad.expect_stdout_line_matches("loaded Zebra state cache")?;
        }

        // Give Zebra enough time to actually write the database to disk.
        tokio::time::sleep(Duration::from_secs(1)).await;

        let logs = zebrad.kill_and_return_output(false)?;

        if !expect_older_version {
            assert!(
                !logs.contains("marked database format as upgraded"),
                "unexpected format upgrade in logs:\n\
                 {logs}"
            );
        }

        if !expect_newer_version {
            assert!(
                !logs.contains("marked database format as downgraded"),
                "unexpected format downgrade in logs:\n\
                 {logs}"
            );
        }

        let output = zebrad.wait_with_output()?;
        let mut output = output.assert_failure()?;

        dir = output
            .take_dir()
            .expect("dir should not already have been taken");

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }
    Ok(())
}

/// Snapshot the `z_getsubtreesbyindex` method in a synchronized chain.
///
/// This test name must have the same prefix as the `lwd_rpc_test`, so they can be run in the same test job.
#[tokio::test]
#[ignore]
async fn fully_synced_rpc_z_getsubtreesbyindex_snapshot_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network,
        "rpc_z_getsubtreesbyindex_sync_snapshots",
        test_type,
        true,
    )? {
        tracing::info!("running fully synced zebrad z_getsubtreesbyindex RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    // It doesn't matter how long the state version upgrade takes,
    // because the sync finished regex is repeated every minute.
    wait_for_state_version_upgrade(
        &mut zebrad,
        &state_version_message,
        state_database_format_version_in_code(),
        None,
    )?;

    // Wait for zebrad to load the full cached blockchain.
    zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

    // Create an http client
    let client =
        RpcRequestClient::new(zebra_rpc_address.expect("already checked that address is valid"));

    // Create test vector matrix
    let zcashd_test_vectors = vec![
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_1".to_string(),
            r#"["sapling", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_0_11".to_string(),
            r#"["sapling", 0, 11]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_17_1".to_string(),
            r#"["sapling", 17, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_sapling_1090_6".to_string(),
            r#"["sapling", 1090, 6]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_0_1".to_string(),
            r#"["orchard", 0, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_338_1".to_string(),
            r#"["orchard", 338, 1]"#.to_string(),
        ),
        (
            "z_getsubtreesbyindex_mainnet_orchard_585_1".to_string(),
            r#"["orchard", 585, 1]"#.to_string(),
        ),
    ];

    for i in zcashd_test_vectors {
        let res = client.call("z_getsubtreesbyindex", i.1).await?;
        let body = res.bytes().await;
        let parsed: Value = serde_json::from_slice(&body.expect("Response is valid json"))?;
        insta::assert_json_snapshot!(i.0, parsed);
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Checks that the Regtest genesis block can be validated.
#[tokio::test]
async fn validate_regtest_genesis_block() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Default::default());
    let state = zebra_state::init_test(&network).await;
    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(zebra_consensus::Config::default(), &network, state)
        .await;

    let genesis_hash = block_verifier_router
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    assert_eq!(
        genesis_hash,
        network.genesis_hash(),
        "validated block hash should match network genesis hash"
    )
}

/// Test that Version messages are sent with the external address when configured to do so.
#[test]
fn external_address() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut external_address_test_config(&Mainnet)?)?;
    let mut child = testdir.spawn_child(args!["start"])?;

    // Give enough time to start connecting to some peers.
    std::thread::sleep(Duration::from_secs(10));

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Zebra started
    output.stdout_line_contains("Starting zebrad")?;

    // Make sure we are using external address for Version messages.
    output.stdout_line_contains("using external address for Version messages")?;

    // Make sure the command was killed.
    output.assert_was_killed()?;

    Ok(())
}

/// Test successful `getblocktemplate` and `submitblock` RPC calls on Regtest on Canopy.
///
/// See [`common::regtest::submit_blocks`] for more information.
// TODO: Test this with an NU5 activation height too once config can be serialized.
#[tokio::test]
async fn regtest_block_templates_are_valid_block_submissions() -> Result<()> {
    common::regtest::submit_blocks_test().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_chain_sync_handles_forks_correctly() -> Result<()> {
    use std::sync::Arc;

    use eyre::Error;
    use tokio::time::timeout;
    use zebra_chain::{chain_tip::ChainTip, primitives::byte_array::increment_big_endian};
    use zebra_rpc::methods::GetBlockHashResponse;
    use zebra_state::{ReadResponse, Response};

    use common::regtest::MiningRpcMethods;

    let _init_guard = zebra_test::init();

    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );
    let mut config = os_assigned_rpc_port_config(false, &net)?;

    config.state.ephemeral = false;
    config.rpc.indexer_listen_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));

    let test_dir = testdir()?.with_config(&mut config)?;
    let mut child = test_dir.spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let indexer_listen_addr = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("starting read state with syncer");
    // Spawn a read state with the RPC syncer to check that it has the same best chain as Zebra
    let (read_state, _latest_chain_tip, mut chain_tip_change, _sync_task) =
        zebra_rpc::sync::init_read_state_with_syncer(
            config.state,
            &config.network.network,
            indexer_listen_addr,
        )
        .await?
        .map_err(|err| eyre!(err))?;

    tracing::info!("waiting for first chain tip change");

    // Wait for Zebrad to start up
    let tip_action = timeout(LAUNCH_DELAY, chain_tip_change.wait_for_tip_change()).await??;
    assert!(
        tip_action.is_reset(),
        "first tip action should be a reset for the genesis block"
    );

    tracing::info!("got genesis chain tip change, submitting more blocks ..");

    let rpc_client = RpcRequestClient::new(rpc_address);
    let mut blocks = Vec::new();
    for _ in 0..10 {
        let (block, height) = rpc_client.block_from_template(&net).await?;

        rpc_client.submit_block(block.clone()).await?;

        blocks.push(block);
        let tip_action = timeout(
            Duration::from_secs(1),
            chain_tip_change.wait_for_tip_change(),
        )
        .await??;

        assert_eq!(
            tip_action.best_tip_height(),
            height,
            "tip action height should match block submission"
        );
    }

    tracing::info!("checking that read state has the new non-finalized best chain blocks");
    for expected_block in blocks.clone() {
        let height = expected_block.coinbase_height().unwrap();
        let zebra_block = rpc_client
            .get_block(height.0 as i32)
            .await
            .map_err(|err| eyre!(err))?
            .expect("Zebra test child should have the expected block");

        assert_eq!(
            zebra_block,
            Arc::new(expected_block),
            "Zebra should have the same block"
        );

        let ReadResponse::Block(read_state_block) = read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(height.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("unexpected read response to a block request")
        };

        assert_eq!(
            zebra_block,
            read_state_block.expect("read state should have the block"),
            "read state should have the same block"
        );
    }

    tracing::info!("getting next block template");
    let (block_11, _) = rpc_client.block_from_template(&net).await?;
    blocks.push(block_11);
    let next_blocks: Vec<_> = blocks.split_off(5);

    tracing::info!("creating populated state");
    let genesis_block = regtest_genesis_block();
    let (state2, read_state2, latest_chain_tip2, _chain_tip_change2) =
        zebra_state::populated_state(
            std::iter::once(genesis_block).chain(blocks.iter().cloned().map(Arc::new)),
            &net,
        )
        .await;

    tracing::info!("attempting to trigger a best chain change");
    for mut block in next_blocks {
        let ReadResponse::ChainInfo(chain_info) = read_state2
            .clone()
            .oneshot(zebra_state::ReadRequest::ChainInfo)
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("wrong response variant");
        };

        let height = block.coinbase_height().unwrap();
        let auth_root = block.auth_data_root();
        let hist_root = chain_info.chain_history_root.unwrap_or_default();
        let header = Arc::make_mut(&mut block.header);

        header.commitment_bytes = match NetworkUpgrade::current(&net, height) {
            NetworkUpgrade::Canopy => hist_root.bytes_in_serialized_order(),
            NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu7 => {
                ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&hist_root, &auth_root)
                    .bytes_in_serialized_order()
            }
            _ => Err(eyre!(
                "Zebra does not support generating pre-Canopy block templates"
            ))?,
        }
        .into();

        increment_big_endian(header.nonce.as_mut());

        header.previous_block_hash = chain_info.tip_hash;

        let Response::Committed(block_hash) = state2
            .clone()
            .oneshot(zebra_state::Request::CommitSemanticallyVerifiedBlock(
                Arc::new(block.clone()).into(),
            ))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("wrong response variant");
        };

        assert!(
            chain_tip_change.last_tip_change().is_none(),
            "there should be no tip change until the last block is submitted"
        );

        rpc_client.submit_block(block.clone()).await?;
        blocks.push(block);
        let best_block_hash: GetBlockHashResponse = rpc_client
            .json_result_from_call("getbestblockhash", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        if block_hash == best_block_hash.hash() {
            break;
        }
    }

    tracing::info!("newly submitted blocks are in the best chain, checking for reset");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let tip_action = timeout(
        Duration::from_secs(1),
        chain_tip_change.wait_for_tip_change(),
    )
    .await??;
    let (expected_height, expected_hash) = latest_chain_tip2
        .best_tip_height_and_hash()
        .expect("should have a chain tip");
    assert!(tip_action.is_reset(), "tip action should be reset");
    assert_eq!(
        tip_action.best_tip_hash_and_height(),
        (expected_hash, expected_height),
        "tip action hashes and heights should match"
    );

    tracing::info!("checking that read state has the new non-finalized best chain blocks");
    for expected_block in blocks {
        let height = expected_block.coinbase_height().unwrap();
        let zebra_block = rpc_client
            .get_block(height.0 as i32)
            .await
            .map_err(|err| eyre!(err))?
            .expect("Zebra test child should have the expected block");

        assert_eq!(
            zebra_block,
            Arc::new(expected_block),
            "Zebra should have the same block"
        );

        let ReadResponse::Block(read_state_block) = read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(height.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            unreachable!("unexpected read response to a block request")
        };

        assert_eq!(
            zebra_block,
            read_state_block.expect("read state should have the block"),
            "read state should have the same block"
        );
    }

    tracing::info!("restarting Zebra on Mainnet");

    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    let mut config = random_known_rpc_port_config(false, &Network::Mainnet)?;
    config.state.ephemeral = false;
    config.rpc.indexer_listen_addr = Some(std::net::SocketAddr::from((
        [127, 0, 0, 1],
        random_known_port(),
    )));
    let indexer_listen_addr = config.rpc.indexer_listen_addr.unwrap();
    let test_dir = testdir()?.with_config(&mut config)?;

    let _child = test_dir.spawn_child(args!["start"])?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("starting read state with syncer");
    // Spawn a read state with the RPC syncer to check that it has the same best chain as Zebra
    let (_read_state, _latest_chain_tip, mut chain_tip_change, _sync_task) =
        zebra_rpc::sync::init_read_state_with_syncer(
            config.state,
            &config.network.network,
            indexer_listen_addr,
        )
        .await?
        .map_err(|err| eyre!(err))?;

    tracing::info!("waiting for finalized chain tip changes");

    timeout(
        Duration::from_secs(200),
        tokio::spawn(async move {
            for _ in 0..2 {
                chain_tip_change
                    .wait_for_tip_change()
                    .await
                    .map_err(|err| eyre!(err))?;
            }

            Ok::<(), Error>(())
        }),
    )
    .await???;

    Ok(())
}

/// Test successful block template submission as a block proposal or submission on a custom Testnet.
///
/// This test can be run locally with:
/// `cargo test --package zebrad --test acceptance -- nu6_funding_streams_and_coinbase_balance --exact --show-output`
#[tokio::test(flavor = "multi_thread")]
async fn nu6_funding_streams_and_coinbase_balance() -> Result<()> {
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        parameters::{
            subsidy::FundingStreamReceiver,
            testnet::{
                self, ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
                ConfiguredFundingStreams,
            },
        },
        serialization::ZcashSerialize,
        work::difficulty::U256,
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::client::HexData;
    use zebra_test::mock_service::MockService;
    let _init_guard = zebra_test::init();

    tracing::info!("running nu6_funding_streams_and_coinbase_balance test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .expect("failed to set genesis hash")
        .with_checkpoints(false)
        .expect("failed to verify checkpoints")
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .expect("failed to set target difficulty limit")
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(1),
            ..Default::default()
        })
        .expect("failed to set activation heights");

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network()
        .expect("failed to build configured network");

    tracing::info!("built configured Testnet, starting state service and block verifier");

    let default_test_config = default_test_config(&network)?;
    let mining_config = default_test_config.mining;
    let miner_address = Address::try_from_zcash_address(
        &network,
        mining_config
            .miner_address
            .clone()
            .expect("mining address should be configured"),
    )
    .expect("configured mining address should be valid");

    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(&network).await;

    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(
        zebra_consensus::Config::default(),
        &network,
        state.clone(),
    )
    .await;

    tracing::info!("started state service and block verifier, committing Regtest genesis block");

    let genesis_hash = block_verifier_router
        .clone()
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    let mut mempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let submitblock_channel = SubmitBlockChannel::new();

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        mining_config,
        false,
        "0.0.1",
        "Zebra tests",
        mempool.clone(),
        state.clone(),
        read_state.clone(),
        block_verifier_router,
        mock_sync_status,
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        Some(submitblock_channel.sender()),
    );

    let make_mock_mempool_request_handler = || async move {
        mempool
            .expect_request(mempool::Request::FullTransactions)
            .await
            .respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                // tip hash needs to match chain info for long poll requests
                last_seen_tip_hash: genesis_hash,
            });
    };

    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;
    let hex_proposal_block = HexData(proposal_block.zcash_serialize_to_vec()?);

    // Check that the block template is a valid block proposal
    let GetBlockTemplateResponse::ProposalMode(block_proposal_result) = rpc
        .get_block_template(Some(GetBlockTemplateParameters::new(
            GetBlockTemplateRequestMode::Proposal,
            Some(hex_proposal_block),
            Default::default(),
            Default::default(),
            Default::default(),
        )))
        .await?
    else {
        panic!(
            "this getblocktemplate call should return the `ProposalMode` variant of the response"
        )
    };

    assert!(
        block_proposal_result.is_valid(),
        "block proposal should succeed"
    );

    // Submit the same block
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    // Check that the submitblock channel received the submitted block
    let mut submit_block_receiver = submitblock_channel.receiver();
    let submit_block_channel_data = submit_block_receiver.recv().await.expect("channel is open");
    assert_eq!(
        submit_block_channel_data,
        (
            proposal_block.hash(),
            proposal_block.coinbase_height().unwrap()
        ),
        "submitblock channel should receive the submitted block"
    );

    // Use an invalid coinbase transaction (with an output value greater than the `block_subsidy + miner_fees - expected_lockbox_funding_stream`)

    let make_configured_recipients_with_lockbox_numerator = |numerator| {
        Some(vec![
            ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Deferred,
                numerator,
                addresses: None,
            },
            ConfiguredFundingStreamRecipient::new_for(FundingStreamReceiver::MajorGrants),
        ])
    };

    // Gets the next block template
    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let valid_original_block_template = block_template.clone();

    let zebra_state::GetBlockTemplateChainInfo {
        chain_history_root, ..
    } = fetch_state_tip_and_local_time(read_state.clone()).await?;

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: make_configured_recipients_with_lockbox_numerator(0),
        }])
        .to_network()
        .expect("failed to build configured network");

    let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
        &network,
        Height(block_template.height()),
        &miner_address,
        &[],
        chain_history_root,
        vec![],
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        None,
    )
    .expect("coinbase transaction should be valid under the given parameters");

    let block_template = BlockTemplateResponse::new(
        block_template.capabilities().clone(),
        block_template.version(),
        block_template.previous_block_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots,
        block_template.transactions().clone(),
        coinbase_txn,
        block_template.long_poll_id(),
        block_template.target(),
        block_template.min_time(),
        block_template.mutable().clone(),
        block_template.nonce_range().clone(),
        block_template.sigop_limit(),
        block_template.size_limit(),
        block_template.cur_time(),
        block_template.bits(),
        block_template.height(),
        block_template.max_time(),
        block_template.submit_old(),
    );

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;

    // Submit the invalid block with an excessive coinbase output value
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    tracing::info!(?submit_block_response, "submitted invalid block");

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::ErrorResponse(SubmitBlockErrorResponse::Rejected),
        "invalid block with excessive coinbase output value should be rejected"
    );

    // Use an invalid coinbase transaction (with an output value less than the `block_subsidy + miner_fees - expected_lockbox_funding_stream`)
    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: make_configured_recipients_with_lockbox_numerator(20),
        }])
        .to_network()
        .expect("failed to build configured network");

    let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
        &network,
        Height(block_template.height()),
        &miner_address,
        &[],
        chain_history_root,
        vec![],
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        None,
    )
    .expect("coinbase transaction should be valid under the given parameters");

    let block_template = BlockTemplateResponse::new(
        block_template.capabilities().clone(),
        block_template.version(),
        block_template.previous_block_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots.block_commitments_hash(),
        default_roots,
        block_template.transactions().clone(),
        coinbase_txn,
        block_template.long_poll_id(),
        block_template.target(),
        block_template.min_time(),
        block_template.mutable().clone(),
        block_template.nonce_range().clone(),
        block_template.sigop_limit(),
        block_template.size_limit(),
        block_template.cur_time(),
        block_template.bits(),
        block_template.height(),
        block_template.max_time(),
        block_template.submit_old(),
    );

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;

    // Submit the invalid block with an excessive coinbase input value
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    tracing::info!(?submit_block_response, "submitted invalid block");

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::ErrorResponse(SubmitBlockErrorResponse::Rejected),
        "invalid block with insufficient coinbase output value should be rejected"
    );

    // Check that the original block template can be submitted successfully
    let proposal_block =
        proposal_block_from_template(&valid_original_block_template, None, &network)?;

    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    Ok(())
}

/// Test successful block template submission as a block proposal.
///
/// This test can be run locally with:
/// `RUSTFLAGS='--cfg zcash_unstable="nu7"' cargo test --package zebrad --test acceptance --features tx_v6 -- nu7_nsm_transactions --exact --show-output`
#[tokio::test(flavor = "multi_thread")]
#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
async fn nu7_nsm_transactions() -> Result<()> {
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        parameters::testnet::{self, ConfiguredActivationHeights, ConfiguredFundingStreams},
        serialization::ZcashSerialize,
        work::difficulty::U256,
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::client::HexData;
    use zebra_test::mock_service::MockService;
    let _init_guard = zebra_test::init();

    tracing::info!("running nu7_nsm_transactions test");

    let base_network_params = testnet::Parameters::build()
        // Regtest genesis hash
        .with_genesis_hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327")
        .with_checkpoints(false)
        .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
        .with_disable_pow(true)
        .with_slow_start_interval(Height::MIN)
        .with_lockbox_disbursements(vec![])
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        });

    let network = base_network_params
        .clone()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(Height(1)..Height(100)),
            // Use default post-NU6 recipients
            recipients: None,
        }])
        .to_network();

    tracing::info!("built configured Testnet, starting state service and block verifier");

    let default_test_config = default_test_config(&network)?;
    let mining_config = default_test_config.mining;

    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(&network).await;

    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init_test(
        zebra_consensus::Config::default(),
        &network,
        state.clone(),
    )
    .await;

    tracing::info!("started state service and block verifier, committing Regtest genesis block");

    let genesis_hash = block_verifier_router
        .clone()
        .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
        .await
        .expect("should validate Regtest genesis block");

    let mut mempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(5))
        .for_unit_tests();
    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let submitblock_channel = SubmitBlockChannel::new();

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        mining_config,
        false,
        "0.0.1",
        "Zebra tests",
        mempool.clone(),
        state.clone(),
        read_state.clone(),
        block_verifier_router,
        mock_sync_status,
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        Some(submitblock_channel.sender()),
    );

    let make_mock_mempool_request_handler = || async move {
        mempool
            .expect_request(mempool::Request::FullTransactions)
            .await
            .respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                // tip hash needs to match chain info for long poll requests
                last_seen_tip_hash: genesis_hash,
            });
    };

    let block_template_fut = rpc.get_block_template(None);
    let mock_mempool_request_handler = make_mock_mempool_request_handler.clone()();
    let (block_template, _) = tokio::join!(block_template_fut, mock_mempool_request_handler);
    let GetBlockTemplateResponse::TemplateMode(block_template) =
        block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!("this getblocktemplate call without parameters should return the `TemplateMode` variant of the response")
    };

    let proposal_block = proposal_block_from_template(&block_template, None, &network)?;
    let hex_proposal_block = HexData(proposal_block.zcash_serialize_to_vec()?);

    // Check that the block template is a valid block proposal
    let GetBlockTemplateResponse::ProposalMode(block_proposal_result) = rpc
        .get_block_template(Some(GetBlockTemplateParameters::new(
            GetBlockTemplateRequestMode::Proposal,
            Some(hex_proposal_block),
            Default::default(),
            Default::default(),
            Default::default(),
        )))
        .await?
    else {
        panic!(
            "this getblocktemplate call should return the `ProposalMode` variant of the response"
        )
    };

    assert!(
        block_proposal_result.is_valid(),
        "block proposal should succeed"
    );

    // Submit the same block
    let submit_block_response = rpc
        .submit_block(HexData(proposal_block.zcash_serialize_to_vec()?), None)
        .await?;

    assert_eq!(
        submit_block_response,
        SubmitBlockResponse::Accepted,
        "valid block should be accepted"
    );

    Ok(())
}

/// Checks that the cached finalized state has the spending transaction ids for every
/// spent outpoint and revealed nullifier in the last 100 blocks of a cached state.
//
// Note: This test is meant to be run locally with a prepared finalized state that
//       has spending transaction ids. This can be done by starting Zebra with the
//       `indexer` feature and waiting until the db format upgrade is complete. It
//       can be undone (removing the indexes) by starting Zebra without the feature
//       and waiting until the db format downgrade is complete.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
#[cfg(feature = "indexer")]
async fn has_spending_transaction_ids() -> Result<()> {
    use std::sync::Arc;
    use tower::Service;
    use zebra_chain::{chain_tip::ChainTip, transparent::Input};
    use zebra_state::{ReadRequest, ReadResponse, SemanticallyVerifiedBlock, Spend};

    use common::cached_state::future_blocks;

    let _init_guard = zebra_test::init();
    let test_type = UpdateZebraCachedStateWithRpc;
    let test_name = "has_spending_transaction_ids_test";
    let network = Mainnet;

    let Some(zebrad_state_path) = test_type.zebrad_state_path(test_name) else {
        // Skip test if there's no cached state.
        return Ok(());
    };

    tracing::info!("loading blocks for non-finalized state");

    let non_finalized_blocks = future_blocks(&network, test_type, test_name, 100).await?;

    let (mut state, mut read_state, latest_chain_tip, _chain_tip_change) =
        common::cached_state::start_state_service_with_cache_dir(&Mainnet, zebrad_state_path)
            .await?;

    tracing::info!("committing blocks to non-finalized state");

    for block in non_finalized_blocks {
        use zebra_state::{CommitSemanticallyVerifiedBlockRequest, MappedRequest};

        let expected_hash = block.hash();
        let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), expected_hash);
        let block_hash = CommitSemanticallyVerifiedBlockRequest(block)
            .mapped_oneshot(&mut state)
            .await
            .map_err(|err| eyre!(err))?;

        assert_eq!(
            expected_hash, block_hash,
            "state should respond with expected block hash"
        );
    }

    let mut tip_hash = latest_chain_tip
        .best_tip_hash()
        .expect("cached state must not be empty");

    tracing::info!("checking indexes of spending transaction ids");

    // Read the last 500 blocks - should be greater than the MAX_BLOCK_REORG_HEIGHT so that
    // both the finalized and non-finalized state are checked.
    let num_blocks_to_check = 500;
    let mut is_failure = false;
    for i in 0..num_blocks_to_check {
        let ReadResponse::Block(block) = read_state
            .ready()
            .await
            .map_err(|err| eyre!(err))?
            .call(ReadRequest::Block(tip_hash.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            panic!("unexpected response to Block request");
        };

        let block = block.expect("should have block with latest_chain_tip hash");

        let spends_with_spending_tx_hashes = block.transactions.iter().cloned().flat_map(|tx| {
            let tx_hash = tx.hash();
            tx.inputs()
                .iter()
                .filter_map(Input::outpoint)
                .map(Spend::from)
                .chain(tx.sprout_nullifiers().cloned().map(Spend::from))
                .chain(tx.sapling_nullifiers().cloned().map(Spend::from))
                .chain(tx.orchard_nullifiers().cloned().map(Spend::from))
                .map(|spend| (spend, tx_hash))
                .collect::<Vec<_>>()
        });

        for (spend, expected_transaction_hash) in spends_with_spending_tx_hashes {
            let ReadResponse::TransactionId(transaction_hash) = read_state
                .ready()
                .await
                .map_err(|err| eyre!(err))?
                .call(ReadRequest::SpendingTransactionId(spend))
                .await
                .map_err(|err| eyre!(err))?
            else {
                panic!("unexpected response to Block request");
            };

            let Some(transaction_hash) = transaction_hash else {
                tracing::warn!(
                    ?spend,
                    depth = i,
                    height = ?block.coinbase_height(),
                    "querying spending tx id for spend failed"
                );
                is_failure = true;
                continue;
            };

            assert_eq!(
                transaction_hash, expected_transaction_hash,
                "spending transaction hash should match expected transaction hash"
            );
        }

        if i % 25 == 0 {
            tracing::info!(
                height = ?block.coinbase_height(),
                "has all spending tx ids at and above block"
            );
        }

        tip_hash = block.header.previous_block_hash;
    }

    assert!(
        !is_failure,
        "at least one spend was missing a spending transaction id"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_and_reconsider_block() -> Result<()> {
    use std::sync::Arc;

    use common::regtest::MiningRpcMethods;

    let _init_guard = zebra_test::init();
    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu7: Some(100),
            ..Default::default()
        }
        .into(),
    );
    let mut config = os_assigned_rpc_port_config(false, &net)?;
    config.state.ephemeral = false;

    let test_dir = testdir()?.with_config(&mut config)?;

    let mut child = test_dir.spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    tracing::info!("waiting for Zebra state cache to be opened");

    tokio::time::sleep(LAUNCH_DELAY).await;

    let rpc_client = RpcRequestClient::new(rpc_address);
    let mut blocks = Vec::new();
    for _ in 0..50 {
        let (block, _) = rpc_client.block_from_template(&net).await?;

        rpc_client.submit_block(block.clone()).await?;
        blocks.push(block);
    }

    tracing::info!("checking that read state has the new non-finalized best chain blocks");
    for expected_block in blocks.clone() {
        let height = expected_block.coinbase_height().unwrap();
        let zebra_block = rpc_client
            .get_block(height.0 as i32)
            .await
            .map_err(|err| eyre!(err))?
            .expect("Zebra test child should have the expected block");

        assert_eq!(
            zebra_block,
            Arc::new(expected_block),
            "Zebra should have the same block"
        );
    }

    tracing::info!("invalidating blocks");

    // Note: This is the block at height 7, it's the 6th generated block.
    let block_6_hash = blocks
        .get(5)
        .expect("should have 50 blocks")
        .hash()
        .to_string();
    let params = serde_json::to_string(&vec![block_6_hash]).expect("should serialize successfully");

    let _: () = rpc_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|err| eyre!(err))?;

    let expected_reconsidered_hashes = blocks
        .iter()
        .skip(5)
        .map(|block| block.hash())
        .collect::<Vec<_>>();

    tracing::info!("reconsidering blocks");

    let reconsidered_hashes: Vec<block::Hash> = rpc_client
        .json_result_from_call("reconsiderblock", &params)
        .await
        .map_err(|err| eyre!(err))?;

    assert_eq!(
        reconsidered_hashes, expected_reconsidered_hashes,
        "reconsidered hashes should match expected hashes"
    );

    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    Ok(())
}

/// Check that Zebra does not depend on any crates from git sources.
#[test]
#[ignore]
fn check_no_git_dependencies() {
    let cargo_lock_contents =
        fs::read_to_string("../Cargo.lock").expect("should have Cargo.lock file in root dir");

    if cargo_lock_contents.contains(r#"source = "git+"#) {
        panic!("Cargo.lock includes git sources")
    }
}

#[tokio::test]
async fn restores_non_finalized_state_and_commits_new_blocks() -> Result<()> {
    let network = Network::new_regtest(Default::default());

    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.state.ephemeral = false;
    let test_dir = testdir()?.with_config(&mut config)?;

    // Start Zebra and generate some blocks.

    tracing::info!("starting Zebra and generating some blocks");
    let mut child = test_dir.spawn_child(args!["start"])?;
    // Avoid dropping the test directory and cleanup of the state cache needed by the next zebrad instance.
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rpc_client = RpcRequestClient::new(rpc_address);
    let generated_block_hashes = rpc_client.generate(50).await?;
    // Wait for non-finalized backup task to make a second write to the backup cache
    tokio::time::sleep(Duration::from_secs(6)).await;

    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad to fully terminate")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Prepare checkpoint heights/hashes
    let last_hash = *generated_block_hashes
        .last()
        .expect("should have at least one block hash");
    let configured_checkpoints = ConfiguredCheckpoints::HeightsAndHashes(vec![
        (Height(0), network.genesis_hash()),
        (Height(50), last_hash),
    ]);

    // Check that Zebra will restore its non-finalized state from backup when the finalized tip is past the
    // max checkpoint height and that it can still commit more blocks to its state.

    tracing::info!("restarting Zebra to check that non-finalized state is restored");
    let mut child = test_dir.spawn_child(args!["start"])?;
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let rpc_client = RpcRequestClient::new(rpc_address);

    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let blockchain_info = rpc_client.blockchain_info().await?;
    tracing::info!(
        ?blockchain_info,
        "got blockchain info after restarting Zebra"
    );

    assert_eq!(
        blockchain_info.best_block_hash(),
        last_hash,
        "tip block hash should match tip hash of previous zebrad instance"
    );

    tracing::info!("checking that Zebra can commit blocks after restoring non-finalized state");
    rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");

    tracing::info!("retrieving blocks to be used with configured checkpoints");
    let checkpointed_blocks = {
        let mut blocks = Vec::new();
        for height in 1..=50 {
            blocks.push(
                rpc_client
                    .get_block(height)
                    .await
                    .map_err(|err| eyre!(err))?
                    .expect("should have block at height"),
            )
        }
        blocks
    };

    tracing::info!(
        "restarting Zebra to check that non-finalized state is _not_ restored when \
         the finalized tip is below the max checkpoint height"
    );
    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad to fully terminate")?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check that the non-finalized state is not restored from backup when the finalized tip height is below the
    // max checkpoint height and that it can still commit more blocks to its state

    tracing::info!("restarting Zebra with configured checkpoints to check that non-finalized state is not restored");
    let network = Network::new_regtest(RegtestParameters {
        checkpoints: Some(configured_checkpoints),
        ..Default::default()
    });
    let mut config = os_assigned_rpc_port_config(false, &network)?;
    config.state.ephemeral = false;
    let mut child = test_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    let test_dir = child.dir.take().expect("should have test directory");
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rpc_client = RpcRequestClient::new(rpc_address);

    assert_eq!(
        rpc_client.blockchain_info().await?.best_block_hash(),
        network.genesis_hash(),
        "Zebra should not restore blocks from non-finalized backup if \
         its finalized tip is below the max checkpoint height"
    );

    let mut submit_block_futs: FuturesUnordered<_> = checkpointed_blocks
        .into_iter()
        .map(Arc::unwrap_or_clone)
        .map(|block| rpc_client.submit_block(block))
        .collect();

    while let Some(result) = submit_block_futs.next().await {
        result?
    }

    // Commit some blocks to check that Zebra's state will still commit blocks, and generate enough blocks
    // for Zebra's finalized tip to pass the max checkpoint height.

    rpc_client
        .generate(200)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)?;
    // Wait for zebrad to fully terminate to ensure database lock is released.
    child
        .wait_with_output()
        .wrap_err("failed to wait for zebrad process to exit after kill")?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check that Zebra will can commit blocks to its state when its finalized tip is past the max checkpoint height
    // and the non-finalized backup cache is disabled or empty.

    tracing::info!(
        "restarting Zebra to check that blocks are committed when the non-finalized state \
         is initially empty and the finalized tip is past the max checkpoint height"
    );
    config.state.should_backup_non_finalized_state = false;
    let mut child = test_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    let rpc_client = RpcRequestClient::new(rpc_address);

    // Wait for Zebra to load its state cache
    tokio::time::sleep(Duration::from_secs(5)).await;

    tracing::info!("checking that Zebra commits blocks with empty non-finalized state");
    rpc_client
        .generate(10)
        .await
        .expect("should successfully commit more blocks to the state");

    child.kill(true)
}

/// Check that Zebra will disconnect from misbehaving peers.
///
/// In order to simulate a misbehaviour peer we start two zebrad instances:
/// - The first one is started with a custom Testnet where PoW is disabled.
/// - The second one is started with the default Testnet where PoW is enabled.
/// The second zebrad instance will connect to the first one, and when the first one mines
/// blocks with invalid PoW the second one should disconnect from it.
#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn disconnects_from_misbehaving_peers() -> Result<()> {
    use std::sync::{atomic::AtomicBool, Arc};

    use common::regtest::MiningRpcMethods;
    use zebra_chain::parameters::testnet::{self, ConfiguredActivationHeights};
    use zebra_rpc::client::PeerInfo;

    let _init_guard = zebra_test::init();
    let network1 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .with_slow_start_interval(Height::MIN)
        .with_disable_pow(true)
        .clear_checkpoints()
        .expect("failed to clear checkpoints")
        .with_network_name("PoWDisabledTestnet")
        .expect("failed to set network name")
        .to_network()
        .expect("failed to build configured network");

    let test_type = LaunchWithEmptyState {
        launches_lightwalletd: false,
    };
    let test_name = "disconnects_from_misbehaving_peers_test";

    if !common::launch::can_spawn_zebrad_for_test_type(test_name, test_type, false) {
        tracing::warn!("skipping disconnects_from_misbehaving_peers test");
        return Ok(());
    }

    // Get the zebrad config
    let mut config = test_type
        .zebrad_config(test_name, false, None, &network1)
        .expect("already checked config")?;

    config.network.cache_dir = false.into();
    config.network.listen_addr = format!("127.0.0.1:{}", random_known_port()).parse()?;
    config.state.ephemeral = true;
    config.network.initial_testnet_peers = [].into();
    config.network.crawl_new_peer_interval = Duration::from_secs(5);

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_1 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on incompatible custom Testnet"
    );

    let is_finished = Arc::new(AtomicBool::new(false));

    {
        let is_finished = Arc::clone(&is_finished);
        let config = config.clone();
        let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraA1".to_string()));
            }

            Ok(())
        });
    }

    let network2 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .with_slow_start_interval(Height::MIN)
        .clear_checkpoints()
        .expect("failed to clear checkpoints")
        .with_network_name("PoWEnabledTestnet")
        .expect("failed to set network name")
        .to_network()
        .expect("failed to build configured network");

    config.network.network = network2;
    config.network.initial_testnet_peers = [config.network.listen_addr.to_string()].into();
    config.network.listen_addr = "127.0.0.1:0".parse()?;
    config.rpc.listen_addr = Some(format!("127.0.0.1:{}", random_known_port()).parse()?);
    config.network.crawl_new_peer_interval = Duration::from_secs(5);
    config.network.cache_dir = false.into();
    config.state.ephemeral = true;

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_2 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on the default Testnet"
    );

    {
        let is_finished = Arc::clone(&is_finished);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let (zebrad_failure_messages, zebrad_ignore_messages) =
                test_type.zebrad_failure_messages();
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraB2".to_string()));
            }

            Ok(())
        });
    }

    tracing::info!("waiting for zebrad nodes to connect");

    // Wait a few seconds for Zebra to start up and make outbound peer connections
    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("checking for peers");

    // Call `getpeerinfo` to check that the zebrad instances have connected
    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    assert!(!peer_info.is_empty(), "should have outbound peer");

    tracing::info!(
        ?peer_info,
        "found peer connection, committing genesis block"
    );

    let genesis_block = network1.block_parsed_iter().next().unwrap();
    rpc_client_1.submit_block(genesis_block.clone()).await?;
    rpc_client_2.submit_block(genesis_block).await?;

    // Call the `generate` method to mine blocks in the zebrad instance where PoW is disabled
    tracing::info!("committed genesis block, mining blocks with invalid PoW");
    tokio::time::sleep(Duration::from_secs(2)).await;

    rpc_client_1.call("generate", "[500]").await?;

    tracing::info!("wait for misbehavior messages to flush into address updater channel");

    tokio::time::sleep(Duration::from_secs(30)).await;

    tracing::info!("calling getpeerinfo to confirm Zebra has dropped the peer connection");

    // Call `getpeerinfo` to check that the zebrad instances have disconnected
    for i in 0..600 {
        let peer_info: Vec<PeerInfo> = rpc_client_2
            .json_result_from_call("getpeerinfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        if peer_info.is_empty() {
            break;
        } else if i % 10 == 0 {
            tracing::info!(?peer_info, "has not yet disconnected from misbehaving peer");
        }

        rpc_client_1.call("generate", "[1]").await?;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    tracing::info!(?peer_info, "called getpeerinfo");

    assert!(peer_info.is_empty(), "should have no peers");

    is_finished.store(true, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}
