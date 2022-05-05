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

use std::{collections::HashSet, convert::TryInto, env, path::PathBuf, time::Duration};

use color_eyre::{
    eyre::{eyre, Result, WrapErr},
    Help,
};

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};
use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::LOCK_FILE_ERROR;

use zebra_test::{args, command::ContextFrom, net::random_known_port, prelude::*};

mod common;

use common::{
    check::{is_zebrad_version, EphemeralCheck, EphemeralConfig},
    config::{default_test_config, persistent_test_config, testdir},
    launch::{
        spawn_zebrad_for_rpc_without_initial_peers, ZebradTestDirExt, BETWEEN_NODES_DELAY,
        LAUNCH_DELAY,
    },
    lightwalletd::{
        random_known_rpc_port_config, zebra_skip_lightwalletd_tests, LightWalletdTestDirExt,
        LightwalletdTestType::{self, *},
        LIGHTWALLETD_DATA_DIR_VAR,
    },
    sync::{
        create_cached_database_height, sync_until, MempoolBehavior, LARGE_CHECKPOINT_TEST_HEIGHT,
        LARGE_CHECKPOINT_TIMEOUT, MEDIUM_CHECKPOINT_TEST_HEIGHT, STOP_AT_HEIGHT_REGEX,
        STOP_ON_LOAD_TIMEOUT, SYNC_FINISHED_REGEX, TINY_CHECKPOINT_TEST_HEIGHT,
        TINY_CHECKPOINT_TIMEOUT,
    },
};

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let child = testdir()?
        .with_config(&mut default_test_config()?)?
        .spawn_child(args!["generate"])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_line_contains("# Default configuration for zebrad")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

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
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(args!["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The first line should have the version
    output.any_output_line(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

    // Make sure we are in help by looking usage string
    output.stdout_line_contains("USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

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
    zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config()?)?;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;

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
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
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

#[test]
fn persistent_mode() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config()?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "state directory empty despite persistent state config"
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

fn ephemeral(cache_dir_config: EphemeralConfig, cache_dir_check: EphemeralCheck) -> Result<()> {
    use std::fs;
    use std::io::ErrorKind;

    zebra_test::init();

    let mut config = default_test_config()?;
    let run_dir = testdir()?;

    let ignored_cache_dir = run_dir.path().join("state");
    if cache_dir_config == EphemeralConfig::MisconfiguredCacheDir {
        // Write a configuration that sets both the cache_dir and ephemeral options
        config.state.cache_dir = ignored_cache_dir.clone();
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
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;
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

            ["state", "zebrad.toml"].iter()
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

            ["zebrad.toml"].iter()
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
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(args![])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_line_contains("USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;

    let child = testdir.spawn_child(args!["version"])?;
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
    zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config()?)?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(args!["version", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["version", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn valid_generated_config_test() -> Result<()> {
    // Unlike the other tests, these tests can not be run in parallel, because
    // they use the generated config. So parallel execution can cause port and
    // cache conflicts.
    valid_generated_config("start", "Starting zebrad")?;

    Ok(())
}

fn valid_generated_config(command: &str, expect_stdout_line_contains: &str) -> Result<()> {
    zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

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

    // Run command using temp dir and kill it after a few seconds
    let mut child = testdir.spawn_child(args![command])?;
    std::thread::sleep(LAUNCH_DELAY);
    child.kill()?;

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

/// Test if `zebrad` can sync the first checkpoint on mainnet.
///
/// The first checkpoint contains a single genesis block.
#[test]
fn sync_one_checkpoint_mainnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
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
#[test]
fn sync_one_checkpoint_testnet() -> Result<()> {
    sync_until(
        TINY_CHECKPOINT_TEST_HEIGHT,
        Testnet,
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
    zebra_test::init();

    restart_stop_at_height_for_network(Network::Mainnet, TINY_CHECKPOINT_TEST_HEIGHT)?;
    restart_stop_at_height_for_network(Network::Testnet, TINY_CHECKPOINT_TEST_HEIGHT)?;

    Ok(())
}

fn restart_stop_at_height_for_network(network: Network, height: block::Height) -> Result<()> {
    let reuse_tempdir = sync_until(
        height,
        network,
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
        network,
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
        Mainnet,
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
        Mainnet,
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
        Mainnet,
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
        Mainnet,
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

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_mainnet() {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "FULL_SYNC_MAINNET_TIMEOUT_MINUTES").expect("unexpected test failure");
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `FULL_SYNC_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn full_sync_testnet() {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Testnet, "FULL_SYNC_TESTNET_TIMEOUT_MINUTES").expect("unexpected test failure");
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
fn full_sync_test(network: Network, timeout_argument_name: &str) -> Result<()> {
    let timeout_argument: Option<u64> = env::var(timeout_argument_name)
        .ok()
        .and_then(|timeout_string| timeout_string.parse().ok());

    if let Some(timeout_minutes) = timeout_argument {
        sync_until(
            block::Height::MAX,
            network,
            SYNC_FINISHED_REGEX,
            Duration::from_secs(60 * timeout_minutes),
            None,
            MempoolBehavior::ShouldAutomaticallyActivate,
            // Use checkpoints to increase full sync performance, and test default Zebra behaviour.
            // (After the changes to the default config in #2368.)
            //
            // TODO: if full validation performance improves, do another test with checkpoint_sync off
            true,
            true,
        )?;
    } else {
        tracing::info!(
            ?network,
            "skipped full sync test, \
             set the {:?} environmental variable to run the test",
            timeout_argument_name,
        );
    }

    Ok(())
}

fn create_cached_database(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height();
    let checkpoint_stop_regex = format!("{}.*CommitFinalized request", STOP_AT_HEIGHT_REGEX);

    create_cached_database_height(
        network,
        height,
        true,
        // Use checkpoints to increase sync performance while caching the database
        true,
        // Check that we're still using checkpoints when we finish the cached sync
        &checkpoint_stop_regex,
    )
}

fn sync_past_mandatory_checkpoint(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height() + 1200;
    let full_validation_stop_regex =
        format!("{}.*best non-finalized chain root", STOP_AT_HEIGHT_REGEX);

    create_cached_database_height(
        network,
        height.unwrap(),
        false,
        // Test full validation by turning checkpoints off
        false,
        &full_validation_stop_regex,
    )
}

// These tests are ignored because they're too long running to run during our
// traditional CI, and they depend on persistent state that cannot be made
// available in github actions or google cloud build. Instead we run these tests
// directly in a vm we spin up on google compute engine, where we can mount
// drives populated by the first two tests, snapshot those drives, and then use
// those to more quickly run the second two tests.

/// Sync up to the mandatory checkpoint height on mainnet and stop.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_mainnet", test)]
fn sync_to_mandatory_checkpoint_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    create_cached_database(network).unwrap();
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_to_mandatory_checkpoint_testnet", test)]
fn sync_to_mandatory_checkpoint_testnet() {
    zebra_test::init();
    let network = Testnet;
    create_cached_database(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_mainnet", test)]
fn sync_past_mandatory_checkpoint_mainnet() {
    zebra_test::init();
    let network = Mainnet;
    sync_past_mandatory_checkpoint(network).unwrap();
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on testnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[cfg_attr(feature = "test_sync_past_mandatory_checkpoint_testnet", test)]
fn sync_past_mandatory_checkpoint_testnet() {
    zebra_test::init();
    let network = Testnet;
    sync_past_mandatory_checkpoint(network).unwrap();
}

#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use hyper::Client;

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{}", port);
    let url = format!("http://{}", endpoint);

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config()?;
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
    child.kill()?;

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
    output.stdout_line_contains(format!("Opened metrics endpoint at {}", endpoint).as_str())?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

#[tokio::test]
async fn tracing_endpoint() -> Result<()> {
    use hyper::{Body, Client, Request};

    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{}", port);
    let url_default = format!("http://{}", endpoint);
    let url_filter = format!("{}/filter", url_default);

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config()?;
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

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_line_contains(format!("Opened tracing endpoint at {}", endpoint).as_str())?;
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

#[tokio::test]
async fn rpc_endpoint() -> Result<()> {
    use hyper::{body::to_bytes, Body, Client, Method, Request};
    use serde_json::Value;

    zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = random_known_rpc_port_config()?;
    let url = format!("http://{}", config.rpc.listen_addr.unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    child.expect_stdout_line_matches(
        format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
    )?;

    // Create an http client
    let client = Client::new();

    // Create a request to call `getinfo` RPC method
    let req = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"jsonrpc":"1.0","method":"getinfo","params":[],"id":123}"#,
        ))?;

    // Make the call to the RPC endpoint
    let res = client.request(req).await?;

    // Test rpc endpoint response
    assert!(res.status().is_success());

    let body = to_bytes(res).await;
    let (body, mut child) = child.kill_on_error(body)?;

    let parsed: Value = serde_json::from_slice(&body)?;

    // Check that we have at least 4 characters in the `build` field.
    let build = parsed["result"]["build"].as_str().unwrap();
    assert!(build.len() > 4, "Got {}", build);

    // Check that the `subversion` field has "Zebra" in it.
    let subversion = parsed["result"]["subversion"].as_str().unwrap();
    assert!(subversion.contains("Zebra"), "Got {}", subversion);

    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Make sure `lightwalletd` works with Zebra, when both their states are empty.
///
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_integration() -> Result<()> {
    lightwalletd_integration_test(LaunchWithEmptyState)
}

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// If `LIGHTWALLETD_DATA_DIR` is set, runs a quick sync, then a full sync.
/// If `LIGHTWALLETD_DATA_DIR` is not set, just runs a full sync.
///
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD`,
/// `ZEBRA_CACHED_STATE_DIR`, and `LIGHTWALLETD_DATA_DIR` env vars are set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_update_sync() -> Result<()> {
    lightwalletd_integration_test(UpdateCachedState)
}

/// Make sure `lightwalletd` can fully sync from genesis using Zebra.
///
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD` and
/// `ZEBRA_CACHED_STATE_DIR` env vars are set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_full_sync() -> Result<()> {
    lightwalletd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    })
}

/// Make sure `lightwalletd` can sync from Zebra, in all available modes.
///
/// Runs the tests in this order:
/// - launch lightwalletd with empty states,
/// - if `ZEBRA_CACHED_STATE_DIR` and `LIGHTWALLETD_DATA_DIR` are set: run a quick update sync,
/// - if `ZEBRA_CACHED_STATE_DIR` is set: run a full sync.
///
/// These tests don't work on Windows, so they are always skipped on that platform.
#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_test_suite() -> Result<()> {
    lightwalletd_integration_test(LaunchWithEmptyState)?;

    // Only runs when ZEBRA_CACHED_STATE_DIR is set.
    // When manually running the test suite, allow cached state in the full sync test.
    lightwalletd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: true,
    })?;

    // Only runs when LIGHTWALLETD_DATA_DIR and ZEBRA_CACHED_STATE_DIR are set
    lightwalletd_integration_test(UpdateCachedState)?;

    Ok(())
}

/// Run a lightwalletd integration test with a configuration for `test_type`.
///
/// Set `allow_cached_state_for_full_sync` to speed up manual full sync tests.
///
/// The random ports in this test can cause [rare port conflicts.](#Note on port conflict)
#[cfg(not(target_os = "windows"))]
fn lightwalletd_integration_test(test_type: LightwalletdTestType) -> Result<()> {
    zebra_test::init();

    // Skip the test unless the user specifically asked for it
    if zebra_skip_lightwalletd_tests() {
        return Ok(());
    }

    // Get the zebrad and lightwalletd configs

    // Handle the Zebra state directory based on the test type:
    // - LaunchWithEmptyState: ignore the state directory
    // - FullSyncFromGenesis & UpdateCachedState:
    //   skip the test if it is not available, timeout if it is not populated

    // Write a configuration that has RPC listen_addr set.
    // If the state path env var is set, use it in the config.
    let config = if let Some(config) = test_type.zebrad_config() {
        config?
    } else {
        return Ok(());
    };

    // Handle the lightwalletd state directory based on the test type:
    // - LaunchWithEmptyState: ignore the state directory
    // - FullSyncFromGenesis: use it if available, timeout if it is already populated
    // - UpdateCachedState: skip the test if it is not available, timeout if it is not populated
    let lightwalletd_state_path = test_type.lightwalletd_state_path();

    if test_type.needs_lightwalletd_cached_state() && lightwalletd_state_path.is_none() {
        tracing::info!(
            "skipped {test_type:?} lightwalletd test, \
             set the {LIGHTWALLETD_DATA_DIR_VAR:?} environment variable to run the test",
        );

        return Ok(());
    }

    tracing::info!(?test_type, "running lightwalletd & zebrad integration test");

    // Get the lists of process failure logs
    let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();

    let (lightwalletd_failure_messages, lightwalletd_ignore_messages) =
        test_type.lightwalletd_failure_messages();

    // Launch zebrad
    let zdir = testdir()?.with_exact_config(&config)?;
    let mut zebrad = zdir
        .spawn_child(args!["start"])?
        .with_timeout(test_type.zebrad_timeout())
        .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

    if test_type.needs_zebra_cached_state() {
        zebrad.expect_stdout_line_matches(r"loaded Zebra state cache tip=.*Height\([0-9]{7}\)")?;
    } else {
        // Timeout the test if we're somehow accidentally using a cached state
        zebrad.expect_stdout_line_matches("loaded Zebra state cache tip=None")?;
    }

    // Wait until `zebrad` has opened the RPC endpoint
    zebrad.expect_stdout_line_matches(regex::escape(
        format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
    ))?;

    // Launch lightwalletd

    // Write a fake zcashd configuration that has the rpcbind and rpcport options set
    let ldir = testdir()?;
    let ldir = ldir.with_lightwalletd_config(config.rpc.listen_addr.unwrap())?;

    // Launch the lightwalletd process
    let lightwalletd = if test_type == LaunchWithEmptyState {
        ldir.spawn_lightwalletd_child(None, args![])?
    } else {
        ldir.spawn_lightwalletd_child(lightwalletd_state_path, args![])?
    };

    let mut lightwalletd = lightwalletd
        .with_timeout(test_type.lightwalletd_timeout())
        .with_failure_regex_iter(lightwalletd_failure_messages, lightwalletd_ignore_messages);

    // Wait until `lightwalletd` has launched
    lightwalletd.expect_stdout_line_matches(regex::escape("Starting gRPC server"))?;

    // Check that `lightwalletd` is calling the expected Zebra RPCs

    // getblockchaininfo
    if test_type.needs_zebra_cached_state() {
        lightwalletd.expect_stdout_line_matches(
            "Got sapling height 419200 block height [0-9]{7} chain main branchID e9ff75a6",
        )?;
    } else {
        // Timeout the test if we're somehow accidentally using a cached state in our temp dir
        lightwalletd.expect_stdout_line_matches(
            "Got sapling height 419200 block height [0-9]{1,6} chain main branchID 00000000",
        )?;
    }

    if test_type.needs_lightwalletd_cached_state() {
        // TODO: expect `[0-9]{7}` when we're using the tip cached state (#4155)
        lightwalletd.expect_stdout_line_matches("Found [0-9]{6,7} blocks in cache")?;
    } else if !test_type.allow_lightwalletd_cached_state() {
        // Timeout the test if we're somehow accidentally using a cached state in our temp dir
        lightwalletd.expect_stdout_line_matches("Found 0 blocks in cache")?;
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
    if test_type.needs_zebra_cached_state() {
        lightwalletd.expect_stdout_line_matches(regex::escape("Ingestor adding block to cache"))?;
    } else {
        lightwalletd.expect_stdout_line_matches(regex::escape(
            "Waiting for zcashd height to reach Sapling activation height (419200)",
        ))?;
    }

    if matches!(test_type, UpdateCachedState | FullSyncFromGenesis { .. }) {
        // Wait for Zebra to sync its cached state to the chain tip
        zebrad.expect_stdout_line_matches(regex::escape("sync_percent=100"))?;

        // Wait for lightwalletd to sync to Zebra's tip
        lightwalletd.expect_stdout_line_matches(regex::escape("Ingestor waiting for block"))?;

        // Check Zebra is still at the tip (also clears and prints Zebra's logs)
        zebrad.expect_stdout_line_matches(regex::escape("sync_percent=100"))?;

        // lightwalletd doesn't log anything when we've reached the tip.
        // But when it gets near the tip, it starts using the mempool.
        lightwalletd.expect_stdout_line_matches(regex::escape(
            "Block hash changed, clearing mempool clients",
        ))?;
        lightwalletd.expect_stdout_line_matches(regex::escape("Adding new mempool txid"))?;
    }

    // Cleanup both processes
    lightwalletd.kill()?;
    zebrad.kill()?;

    let lightwalletd_output = lightwalletd.wait_with_output()?.assert_failure()?;
    let zebrad_output = zebrad.wait_with_output()?.assert_failure()?;

    // If the test fails here, see the [note on port conflict](#Note on port conflict)
    //
    // zcash/lightwalletd exits by itself, but
    // adityapk00/lightwalletd keeps on going, so it gets killed by the test harness.

    lightwalletd_output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    zebrad_output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test will start 2 zebrad nodes one after the other using the same Zcash listener.
/// It is expected that the first node spawned will get exclusive use of the port.
/// The second node will panic with the Zcash listener conflict hint added in #1535.
#[test]
fn zebra_zcash_listener_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created network listen_addr
    let mut config = default_test_config()?;
    config.network.listen_addr = listen_addr.parse().unwrap();
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(
        "Opened Zcash protocol endpoint at {}",
        listen_addr
    ));

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
fn zebra_metrics_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created metrics endpoint_addr
    let mut config = default_test_config()?;
    config.metrics.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened metrics endpoint at {}", listen_addr));

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
fn zebra_tracing_conflict() -> Result<()> {
    zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{}", port);

    // Write a configuration that has our created tracing endpoint_addr
    let mut config = default_test_config()?;
    config.tracing.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened tracing endpoint at {}", listen_addr));

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
#[test]
#[cfg(not(target_os = "windows"))]
fn zebra_rpc_conflict() -> Result<()> {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = random_known_rpc_port_config()?;

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
    zebra_test::init();

    // A persistent config has a fixed temp state directory, but asks the OS to
    // automatically choose an unused port
    let mut config = persistent_test_config()?;
    let dir_conflict = testdir()?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let contains = if cfg!(unix) {
        let mut dir_conflict_full = PathBuf::new();
        dir_conflict_full.push(dir_conflict.path());
        dir_conflict_full.push("state");
        dir_conflict_full.push(format!(
            "v{}",
            zebra_state::constants::DATABASE_FORMAT_VERSION
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
fn check_config_conflict<T, U>(
    first_dir: T,
    first_stdout_regex: &str,
    second_dir: U,
    second_stderr_regex: &str,
) -> Result<()>
where
    T: ZebradTestDirExt,
    U: ZebradTestDirExt,
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
    let node1_kill_res = node1.kill();
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
    zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = LightwalletdTestType::FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    };

    // Handle the Zebra state directory
    let cached_state_path = test_type.zebrad_state_path();

    if cached_state_path.is_none() {
        tracing::info!("skipping fully synced zebrad RPC test");
        return Ok(());
    };

    tracing::info!("running fully synced zebrad RPC test");

    let network = Network::Mainnet;

    let (_zebrad, zebra_rpc_address) = spawn_zebrad_for_rpc_without_initial_peers(
        network,
        cached_state_path.unwrap(),
        test_type.zebrad_timeout(),
    )?;

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let client = reqwest::Client::new();
    let res = client
        .post(format!("http://{}", &zebra_rpc_address.to_string()))
        // Manually constructed request to avoid encoding it, for simplicity
        .body(r#"{"jsonrpc": "2.0", "method": "getblock", "params": ["1180900", 0], "id":123 }"#)
        .header("Content-Type", "application/json")
        .send()
        .await?
        .text()
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    let expected_bytes = zebra_test::vectors::MAINNET_BLOCKS
        .get(&1_180_900)
        .expect("test block must exist");
    let expected_hex = hex::encode(expected_bytes);
    assert!(
        res.contains(&expected_hex),
        "response did not contain the desired block: {}",
        res
    );

    Ok(())
}

/// Test sending transactions using a lightwalletd instance connected to a zebrad instance.
///
/// See [`common::lightwalletd::send_transaction_test`] for more information.
#[tokio::test]
async fn sending_transactions_using_lightwalletd() -> Result<()> {
    common::lightwalletd::send_transaction_test::run().await
}

/// Test all the rpc methods a wallet connected to lightwalletd can call.
///
/// See [`common::lightwalletd::wallet_grpc_test`] for more information.
#[tokio::test]
async fn lightwalletd_wallet_grpc_tests() -> Result<()> {
    common::lightwalletd::wallet_grpc_test::run().await
}
