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
//! by setting the `ZEBRA_SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, IPv4 localhost is not on `127.0.0.1`,
//! or you have poor network connectivity,
//! skip all the network tests by setting the `ZEBRA_SKIP_NETWORK_TESTS` environmental variable.
//!
//! ## Large/full sync tests
//!
//! This file has sync tests that are marked as ignored because they take too much time to run.
//! Some of them require environment variables or directories to be present:
//!
//! - `FULL_SYNC_MAINNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//! will allow this test to run or give up. Value for the Mainnet full sync tests.
//! - `FULL_SYNC_TESTNET_TIMEOUT_MINUTES` env variable: The total number of minutes we
//! will allow this test to run or give up. Value for the Testnet ful  sync tests.
//! - `/zebrad-cache` directory: For some sync tests, this needs to be created in
//! the file system, the created directory should have write permissions.
//!
//! Here are some examples on how to run each of the tests:
//!
//! ```console
//! $ cargo test sync_large_checkpoints_mainnet -- --ignored --nocapture
//!
//! $ cargo test sync_large_checkpoints_mempool_mainnet -- --ignored --nocapture
//!
//! $ sudo mkdir /zebrad-cache
//! $ sudo chmod 777 /zebrad-cache
//! $ export FULL_SYNC_MAINNET_TIMEOUT_MINUTES=600
//! $ cargo test full_sync_mainnet -- --ignored --nocapture
//!
//! $ sudo mkdir /zebrad-cache
//! $ sudo chmod 777 /zebrad-cache
//! $ export FULL_SYNC_TESTNET_TIMEOUT_MINUTES=600
//! $ cargo test full_sync_testnet -- --ignored --nocapture
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Lightwalletd tests
//!
//! The lightwalletd software is an interface service that uses zebrad or zcashd RPC methods to serve wallets or other applications with blockchain content in an efficient manner.
//! There are several versions of lightwalled in the form of different forks. The original
//! repo is <https://github.com/zcash/lightwalletd>, zecwallet Lite uses a custom fork: <https://github.com/adityapk00/lightwalletd>.
//! Initially this tests were made with `adityapk00/lightwalletd` fork but changes for fast spendability support had
//! been made to `zcash/lightwalletd` only.
//!
//! We expect `adityapk00/lightwalletd` to remain working with Zebra but for this tests we are using `zcash/lightwalletd`.
//!
//! Zebra lightwalletd tests are not all marked as ignored but none will run unless
//! at least the `ZEBRA_TEST_LIGHTWALLETD` environment variable is present:
//!
//! - `ZEBRA_TEST_LIGHTWALLETD` env variable: Needs to be present to run any of the lightwalletd tests.
//! - `ZEBRA_CACHED_STATE_DIR` env var: The path to a zebra blockchain database.
//! - `LIGHTWALLETD_DATA_DIR` env variable. The path to a lightwalletd database.
//! - `--features lightwalletd-grpc-tests` cargo flag. The flag given to cargo to build the source code of the running test.
//!
//! Here are some examples of running each test:
//!
//! ```console
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ cargo test lightwalletd_integration -- --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! $ export LIGHTWALLETD_DATA_DIR="/path/to/lightwalletd/database"
//! $ cargo test lightwalletd_update_sync -- --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! $ cargo test lightwalletd_full_sync -- --ignored --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ cargo test lightwalletd_test_suite -- --ignored --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! $ cargo test fully_synced_rpc_test -- --ignored --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! $ export LIGHTWALLETD_DATA_DIR="/path/to/lightwalletd/database"
//! $ cargo test sending_transactions_using_lightwalletd --features lightwalletd-grpc-tests -- --ignored --nocapture
//!
//! $ export ZEBRA_TEST_LIGHTWALLETD=true
//! $ export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! $ export LIGHTWALLETD_DATA_DIR="/path/to/lightwalletd/database"
//! $ cargo test lightwalletd_wallet_grpc_tests --features lightwalletd-grpc-tests -- --ignored --nocapture
//! ```
//!
//! ## Getblocktemplate tests
//!
//! Example of how to run the get_block_template test:
//!
//! ```console
//! ZEBRA_CACHED_STATE_DIR=/path/to/zebra/state cargo test get_block_template --features getblocktemplate-rpcs --release -- --ignored --nocapture
//! ```
//!
//! Example of how to run the submit_block test:
//!
//! ```console
//! ZEBRA_CACHED_STATE_DIR=/path/to/zebra/state cargo test submit_block --features getblocktemplate-rpcs --release -- --ignored --nocapture
//! ```
//!
//! Please refer to the documentation of each test for more information.
//!
//! ## Shielded scanning tests
//!
//! Example of how to run the scan_task_commands test:
//!
//! ```console
//! ZEBRA_CACHED_STATE_DIR=/path/to/zebra/state cargo test scan_task_commands --features shielded-scan --release -- --ignored --nocapture
//! ```
//!
//! ## Checkpoint Generation Tests
//!
//! Generate checkpoints on mainnet and testnet using a cached state:
//! ```console
//! GENERATE_CHECKPOINTS_MAINNET=1 ENTRYPOINT_FEATURES=zebra-checkpoints ZEBRA_CACHED_STATE_DIR=/path/to/zebra/state docker/entrypoint.sh
//! GENERATE_CHECKPOINTS_TESTNET=1 ENTRYPOINT_FEATURES=zebra-checkpoints ZEBRA_CACHED_STATE_DIR=/path/to/zebra/state docker/entrypoint.sh
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

use std::{
    cmp::Ordering,
    collections::HashSet,
    env, fs, panic,
    path::PathBuf,
    time::{Duration, Instant},
};

use color_eyre::{
    eyre::{eyre, WrapErr},
    Help,
};
use semver::Version;
use serde_json::Value;

use tower::ServiceExt;
use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Height},
    parameters::Network::{self, *},
};
use zebra_consensus::ParameterCheckpoint;
use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{constants::LOCK_FILE_ERROR, state_database_format_version_in_code};

use zebra_test::{
    args,
    command::{to_regex::CollectRegexSet, ContextFrom},
    net::random_known_port,
    prelude::*,
};

mod common;

use common::{
    check::{is_zebrad_version, EphemeralCheck, EphemeralConfig},
    config::random_known_rpc_port_config,
    config::{
        config_file_full_path, configs_dir, default_test_config, external_address_test_config,
        persistent_test_config, testdir,
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

use crate::common::cached_state::{
    wait_for_state_version_message, wait_for_state_version_upgrade, DATABASE_FORMAT_UPGRADE_IS_LONG,
};

/// The maximum amount of time that we allow the creation of a future to block the `tokio` executor.
///
/// This should be larger than the amount of time between thread time slices on a busy test VM.
///
/// This limit only applies to some tests.
pub const MAX_ASYNC_BLOCKING_TIME: Duration = zebra_test::mock_service::DEFAULT_MAX_REQUEST_DELAY;

/// The test config file prefix for `--feature getblocktemplate-rpcs` configs.
pub const GET_BLOCK_TEMPLATE_CONFIG_PREFIX: &str = "getblocktemplate-";

/// The test config file prefix for `--feature shielded-scan` configs.
pub const SHIELDED_SCAN_CONFIG_PREFIX: &str = "shieldedscan-";

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
    let db_init_handle = zebra_state::spawn_init(
        config.state.clone(),
        &config.network.network,
        Height::MAX,
        0,
    );

    // it's faster to panic if it takes longer than expected, since the executor
    // will wait indefinitely for blocking operation to finish once started
    let block_duration = start.elapsed();
    assert!(
        block_duration <= MAX_ASYNC_BLOCKING_TIME,
        "futures executor was blocked longer than expected ({block_duration:?})",
    );

    db_init_handle.await?;

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

    let expected_run_dir_file_names = match cache_dir_check {
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

            ["state", "network", "zebrad.toml"].iter()
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

            ["network", "zebrad.toml"].iter()
        }
    };

    let expected_run_dir_file_names = expected_run_dir_file_names.map(Into::into).collect();
    let run_dir_file_names = run_dir
        .path()
        .read_dir()
        .expect("run_dir should still exist")
        .map(|dir_entry| dir_entry.expect("run_dir is readable").file_name())
        // ignore directory list order, because it can vary based on the OS and filesystem
        .collect::<HashSet<_>>();

    assert_with_context!(
        run_dir_file_names == expected_run_dir_file_names,
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
         zebrad/tests/common/configs/{}<next-release-tag>.toml \n\
         \n\
         Or run: \n\
         cargo build {}--bin zebrad && \n\
         zebrad generate | \n\
         sed 's/cache_dir = \".*\"/cache_dir = \"cache_dir\"/' > \n\
         zebrad/tests/common/configs/{}<next-release-tag>.toml",
        if cfg!(feature = "shielded-scan") {
            SHIELDED_SCAN_CONFIG_PREFIX
        } else {
            ""
        },
        if cfg!(feature = "shielded-scan") {
            "--features=shielded-scan "
        } else {
            ""
        },
        if cfg!(feature = "shielded-scan") {
            SHIELDED_SCAN_CONFIG_PREFIX
        } else {
            ""
        },
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
    output.stderr_contains("Zebra could not parse the provided config file. This might mean you are using a deprecated format of the file.")?;

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

        // ignore files starting with getblocktemplate prefix
        // if we were not built with the getblocktemplate-rpcs feature.
        #[cfg(not(feature = "getblocktemplate-rpcs"))]
        if config_file_name.starts_with(GET_BLOCK_TEMPLATE_CONFIG_PREFIX) {
            tracing::info!(
                ?config_file_path,
                "skipping getblocktemplate-rpcs config file path"
            );
            continue;
        }

        // ignore files starting with shieldedscan prefix
        // if we were not built with the shielded-scan feature.
        #[cfg(not(feature = "shielded-scan"))]
        if config_file_name.starts_with(SHIELDED_SCAN_CONFIG_PREFIX) {
            tracing::info!(?config_file_path, "skipping shielded-scan config file path");
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

        // ignore files starting with getblocktemplate prefix
        // if we were not built with the getblocktemplate-rpcs feature.
        #[cfg(not(feature = "getblocktemplate-rpcs"))]
        if config_file_name.starts_with(GET_BLOCK_TEMPLATE_CONFIG_PREFIX) {
            tracing::info!(
                ?config_file_path,
                "skipping getblocktemplate-rpcs config file path"
            );
            continue;
        }

        // ignore files starting with shieldedscan prefix
        // if we were not built with the shielded-scan feature.
        #[cfg(not(feature = "shielded-scan"))]
        if config_file_name.starts_with(SHIELDED_SCAN_CONFIG_PREFIX) {
            tracing::info!(?config_file_path, "skipping shielded-scan config file path");
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
            format!(
                "loaded zebrad config.*config_path.*=.*{}",
                regex::escape(config_file_name)
            ),
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
fn sync_large_checkpoints_mainnet() -> Result<()> {
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

// TODO: We had `sync_large_checkpoints_testnet` and `sync_large_checkpoints_mempool_testnet`,
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
    let height = network.mandatory_checkpoint_height() + 1200;
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
    // - the path from ZEBRA_CACHED_STATE_DIR
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
        eprintln!(
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
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_mainnet", test)]
fn sync_to_mandatory_checkpoint_mainnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    create_cached_database(network)
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_testnet", test)]
fn sync_to_mandatory_checkpoint_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    create_cached_database(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_mainnet", test)]
fn sync_past_mandatory_checkpoint_mainnet() -> Result<()> {
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
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_testnet", test)]
fn sync_past_mandatory_checkpoint_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    sync_past_mandatory_checkpoint(network)
}

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_mainnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "FULL_SYNC_MAINNET_TIMEOUT_MINUTES")
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_testnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(
        Network::new_default_testnet(),
        "FULL_SYNC_TESTNET_TIMEOUT_MINUTES",
    )
}

#[cfg(all(feature = "prometheus", not(target_os = "windows")))]
#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use hyper::Client;

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
    let client = Client::new();

    // Test metrics endpoint
    let res = client.get(url.try_into().expect("url is valid")).await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await;
    let (body, mut child) = child.kill_on_error(body)?;
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
    use hyper::{Body, Client, Request};

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
    let client = Client::new();

    // Test tracing endpoint
    let res = client
        .get(url_default.try_into().expect("url_default is valid"))
        .await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());
    let body = hyper::body::to_bytes(res).await;
    let (body, child) = child.kill_on_error(body)?;

    // Set a filter and make sure it was changed
    let request = Request::post(url_filter.clone())
        .body(Body::from("zebrad=debug"))
        .unwrap();
    let post = client.request(request).await;
    let (_post, child) = child.kill_on_error(post)?;

    let tracing_res = client
        .get(url_filter.try_into().expect("url_filter is valid"))
        .await;
    let (tracing_res, child) = child.kill_on_error(tracing_res)?;
    assert!(tracing_res.status().is_success());
    let tracing_body = hyper::body::to_bytes(tracing_res).await;
    let (tracing_body, mut child) = child.kill_on_error(tracing_body)?;

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
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in tracing filter response");

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
#[cfg(not(target_os = "windows"))]
async fn rpc_endpoint_single_thread() -> Result<()> {
    rpc_endpoint(false).await
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with multiple threads.
#[tokio::test]
#[cfg(not(target_os = "windows"))]
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
    let mut config = random_known_rpc_port_config(parallel_cpu_threads, &Mainnet)?;

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    child.expect_stdout_line_matches(
        format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
    )?;

    // Create an http client
    let client = RpcRequestClient::new(config.rpc.listen_addr.unwrap());

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
#[cfg(not(target_os = "windows"))]
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
    child.expect_stdout_line_matches(
        format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
    )?;

    // Create an http client
    let client = RpcRequestClient::new(config.rpc.listen_addr.unwrap());

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
        let _init_guard = zebra_test::init();

        // Write a configuration that has RPC listen_addr set
        // [Note on port conflict](#Note on port conflict)
        let mut config = random_known_rpc_port_config(false, &Mainnet)?;
        config.tracing.filter = Some("trace".to_string());
        config.tracing.buffer_limit = 100;
        let zebra_rpc_address = config.rpc.listen_addr.unwrap();

        let dir = testdir()?.with_config(&mut config)?;
        let mut child = dir
            .spawn_child(args!["start"])?
            .with_timeout(TINY_CHECKPOINT_TIMEOUT);

        // Wait until port is open.
        child.expect_stdout_line_matches(
            format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
        )?;

        // Create an http client
        let client = RpcRequestClient::new(zebra_rpc_address);

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
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_integration() -> Result<()> {
    lightwalletd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })
}

/// Make sure `zebrad` can sync from peers, but don't actually launch `lightwalletd`.
///
/// This test only runs when the `ZEBRA_CACHED_STATE_DIR` env var is set.
///
/// This test might work on Windows.
#[test]
fn zebrad_update_sync() -> Result<()> {
    lightwalletd_integration_test(UpdateZebraCachedStateNoRpc)
}

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// This test only runs when:
/// - the `ZEBRA_TEST_LIGHTWALLETD`, `ZEBRA_CACHED_STATE_DIR`, and
///   `LIGHTWALLETD_DATA_DIR` env vars are set, and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lightwalletd_update_sync() -> Result<()> {
    lightwalletd_integration_test(UpdateCachedState)
}

/// Make sure `lightwalletd` can fully sync from genesis using Zebra.
///
/// This test only runs when:
/// - the `ZEBRA_TEST_LIGHTWALLETD` and `ZEBRA_CACHED_STATE_DIR` env vars are set, and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lightwalletd_full_sync() -> Result<()> {
    lightwalletd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    })
}

/// Make sure `lightwalletd` can sync from Zebra, in all available modes.
///
/// Runs the tests in this order:
/// - launch lightwalletd with empty states,
/// - if `ZEBRA_CACHED_STATE_DIR` is set:
///   - run a full sync
/// - if `ZEBRA_CACHED_STATE_DIR` and `LIGHTWALLETD_DATA_DIR` are set:
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
    lightwalletd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })?;

    // Only runs when ZEBRA_CACHED_STATE_DIR is set.
    lightwalletd_integration_test(UpdateZebraCachedStateNoRpc)?;

    // These tests need the compile-time gRPC feature
    #[cfg(feature = "lightwalletd-grpc-tests")]
    {
        // Do the quick tests first

        // Only runs when LIGHTWALLETD_DATA_DIR and ZEBRA_CACHED_STATE_DIR are set
        lightwalletd_integration_test(UpdateCachedState)?;

        // Only runs when LIGHTWALLETD_DATA_DIR and ZEBRA_CACHED_STATE_DIR are set
        common::lightwalletd::wallet_grpc_test::run().await?;

        // Then do the slow tests

        // Only runs when ZEBRA_CACHED_STATE_DIR is set.
        // When manually running the test suite, allow cached state in the full sync test.
        lightwalletd_integration_test(FullSyncFromGenesis {
            allow_lightwalletd_cached_state: true,
        })?;

        // Only runs when LIGHTWALLETD_DATA_DIR and ZEBRA_CACHED_STATE_DIR are set
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
fn lightwalletd_integration_test(test_type: TestType) -> Result<()> {
    let _init_guard = zebra_test::init();

    // We run these sync tests with a network connection, for better test coverage.
    let use_internet_connection = true;
    let network = Mainnet;
    let test_name = "lightwalletd_integration_test";

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
    if test_type.launches_lightwalletd() && !DATABASE_FORMAT_UPGRADE_IS_LONG {
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

    check_config_conflict(dir1, regex1.as_str(), dir2, "Unable to start RPC server")?;

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
async fn fully_synced_rpc_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateCachedState;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, "fully_synced_rpc_test", test_type, false)?
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
async fn sending_transactions_using_lightwalletd() -> Result<()> {
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
async fn lightwalletd_wallet_grpc_tests() -> Result<()> {
    common::lightwalletd::wallet_grpc_test::run().await
}

/// Test successful getpeerinfo rpc call
///
/// See [`common::get_block_template_rpcs::get_peer_info`] for more information.
#[tokio::test]
#[cfg(feature = "getblocktemplate-rpcs")]
async fn get_peer_info() -> Result<()> {
    common::get_block_template_rpcs::get_peer_info::run().await
}

/// Test successful getblocktemplate rpc call
///
/// See [`common::get_block_template_rpcs::get_block_template`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "getblocktemplate-rpcs")]
async fn get_block_template() -> Result<()> {
    common::get_block_template_rpcs::get_block_template::run().await
}

/// Test successful submitblock rpc call
///
/// See [`common::get_block_template_rpcs::submit_block`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "getblocktemplate-rpcs")]
async fn submit_block() -> Result<()> {
    common::get_block_template_rpcs::submit_block::run().await
}

/// Check that the the end of support code is called at least once.
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
/// This test name must have the same prefix as the `fully_synced_rpc_test`, so they can be run in the same test job.
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

/// Test that the scanner task gets started when the node starts.
#[test]
#[cfg(feature = "shielded-scan")]
fn scan_task_starts() -> Result<()> {
    use indexmap::IndexMap;
    use zebra_scan::tests::ZECPAGES_SAPLING_VIEWING_KEY;

    let _init_guard = zebra_test::init();

    let test_type = TestType::LaunchWithEmptyState {
        launches_lightwalletd: false,
    };
    let mut config = default_test_config(&Mainnet)?;
    let mut keys = IndexMap::new();
    keys.insert(ZECPAGES_SAPLING_VIEWING_KEY.to_string(), 1);
    config.shielded_scan.sapling_keys_to_scan = keys;

    // Start zebra with the config.
    let mut zebrad = testdir()?
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .with_timeout(test_type.zebrad_timeout());

    // Check scanner was started.
    zebrad.expect_stdout_line_matches("loaded Zebra scanner cache")?;

    // Look for 2 scanner notices indicating we are below sapling activation.
    zebrad.expect_stdout_line_matches("scanner is waiting for Sapling activation. Current tip: [0-9]{1,4}, Sapling activation: 419200")?;
    zebrad.expect_stdout_line_matches("scanner is waiting for Sapling activation. Current tip: [0-9]{1,4}, Sapling activation: 419200")?;

    // Kill the node.
    zebrad.kill(false)?;

    // Check that scan task started and the first scanning is done.
    let output = zebrad.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;
    output.assert_failure()?;

    Ok(())
}

/// Test that the scanner gRPC server starts when the node starts.
#[tokio::test]
#[cfg(all(feature = "shielded-scan", not(target_os = "windows")))]
async fn scan_rpc_server_starts() -> Result<()> {
    use zebra_grpc::scanner::{scanner_client::ScannerClient, Empty};

    let _init_guard = zebra_test::init();

    let test_type = TestType::LaunchWithEmptyState {
        launches_lightwalletd: false,
    };

    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");
    let mut config = default_test_config(&Mainnet)?;
    config.shielded_scan.listen_addr = Some(listen_addr.parse()?);

    // Start zebra with the config.
    let mut zebrad = testdir()?
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .with_timeout(test_type.zebrad_timeout());

    // Wait until gRPC server is starting.
    tokio::time::sleep(LAUNCH_DELAY).await;
    zebrad.expect_stdout_line_matches("starting scan gRPC server")?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut client = ScannerClient::connect(format!("http://{listen_addr}")).await?;

    let request = tonic::Request::new(Empty {});

    client.get_info(request).await?;

    // Kill the node.
    zebrad.kill(false)?;

    // Check that scan task started and the first scanning is done.
    let output = zebrad.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;
    output.assert_failure()?;

    Ok(())
}

/// Test that the scanner can continue scanning where it was left when zebrad restarts.
///
/// Needs a cache state close to the tip. A possible way to run it locally is:
///
/// export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
/// cargo test scan_start_where_left --features="shielded-scan" -- --ignored --nocapture
///
/// The test will run zebrad with a key to scan, scan the first few blocks after sapling and then stops.
/// Then it will restart zebrad and check that it resumes scanning where it was left.
///
/// Note: This test will remove all the contents you may have in the ZEBRA_CACHED_STATE_DIR/private-scan directory
/// so it can start with an empty scanning state.
#[ignore]
#[test]
#[cfg(feature = "shielded-scan")]
fn scan_start_where_left() -> Result<()> {
    use indexmap::IndexMap;
    use zebra_scan::{storage::db::SCANNER_DATABASE_KIND, tests::ZECPAGES_SAPLING_VIEWING_KEY};

    let _init_guard = zebra_test::init();

    // use `UpdateZebraCachedStateNoRpc` as the test type to make sure a zebrad cache state is available.
    let test_type = TestType::UpdateZebraCachedStateNoRpc;
    if let Some(cache_dir) = test_type.zebrad_state_path("scan test") {
        // Add a key to the config
        let mut config = default_test_config(&Mainnet)?;
        let mut keys = IndexMap::new();
        keys.insert(ZECPAGES_SAPLING_VIEWING_KEY.to_string(), 1);
        config.shielded_scan.sapling_keys_to_scan = keys;

        // Add the cache dir to shielded scan, make it the same as the zebrad cache state.
        config
            .shielded_scan
            .db_config_mut()
            .cache_dir
            .clone_from(&cache_dir);
        config.shielded_scan.db_config_mut().ephemeral = false;

        // Add the cache dir to state.
        config.state.cache_dir.clone_from(&cache_dir);
        config.state.ephemeral = false;

        // Remove the scan directory before starting.
        let scan_db_path = cache_dir.join(SCANNER_DATABASE_KIND);
        fs::remove_dir_all(std::path::Path::new(&scan_db_path)).ok();

        // Start zebra with the config.
        let mut zebrad = testdir()?
            .with_exact_config(&config)?
            .spawn_child(args!["start"])?
            .with_timeout(test_type.zebrad_timeout());

        // Check scanner was started.
        zebrad.expect_stdout_line_matches("loaded Zebra scanner cache")?;

        // The first time
        zebrad.expect_stdout_line_matches(
            r"Scanning the blockchain for key 0, started at block 419200, now at block 420000",
        )?;

        // Make sure scanner scans a few blocks.
        zebrad.expect_stdout_line_matches(
            r"Scanning the blockchain for key 0, started at block 419200, now at block 430000",
        )?;
        zebrad.expect_stdout_line_matches(
            r"Scanning the blockchain for key 0, started at block 419200, now at block 440000",
        )?;

        // Kill the node.
        zebrad.kill(false)?;
        let output = zebrad.wait_with_output()?;

        // Make sure the command was killed
        output.assert_was_killed()?;
        output.assert_failure()?;

        // Start the node again.
        let mut zebrad = testdir()?
            .with_exact_config(&config)?
            .spawn_child(args!["start"])?
            .with_timeout(test_type.zebrad_timeout());

        // Resuming message.
        zebrad.expect_stdout_line_matches(
            "Last scanned height for key number 0 is 439000, resuming at 439001",
        )?;
        zebrad.expect_stdout_line_matches("loaded Zebra scanner cache")?;

        // Start scanning where it was left.
        zebrad.expect_stdout_line_matches(
            r"Scanning the blockchain for key 0, started at block 439001, now at block 440000",
        )?;
        zebrad.expect_stdout_line_matches(
            r"Scanning the blockchain for key 0, started at block 439001, now at block 450000",
        )?;
    }

    Ok(())
}

// TODO: Add this test to CI (#8236)
/// Tests successful:
/// - Registration of a new key,
/// - Subscription to scan results of new key, and
/// - Deletion of keys
/// in the scan task
/// See [`common::shielded_scan::scan_task_commands`] for more information.
#[tokio::test]
#[ignore]
#[cfg(feature = "shielded-scan")]
async fn scan_task_commands() -> Result<()> {
    common::shielded_scan::scan_task_commands::run().await
}

/// Checks that the Regtest genesis block can be validated.
#[tokio::test]
async fn validate_regtest_genesis_block() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(None);
    let state = zebra_state::init_test(&network);
    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init(zebra_consensus::Config::default(), &network, state).await;

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
#[cfg(feature = "getblocktemplate-rpcs")]
async fn regtest_submit_blocks() -> Result<()> {
    common::regtest::submit_blocks_test().await?;
    Ok(())
}
