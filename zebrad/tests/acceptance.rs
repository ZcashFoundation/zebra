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

use std::{
    collections::HashSet, convert::TryInto, env, net::SocketAddr, path::PathBuf, time::Duration,
};

use color_eyre::{
    eyre::{Result, WrapErr},
    Help,
};

use zebra_chain::{
    block::Height,
    parameters::Network::{self, *},
};
use zebra_network::constants::PORT_IN_USE_ERROR;
use zebra_state::constants::LOCK_FILE_ERROR;

use zebra_test::{
    args,
    command::{ContextFrom, NO_MATCHES_REGEX_ITER},
    net::random_known_port,
    prelude::*,
};

mod common;

use common::{
    check::{is_zebrad_version, EphemeralCheck, EphemeralConfig},
    config::{default_test_config, persistent_test_config, testdir},
    launch::{ZebradTestDirExt, BETWEEN_NODES_DELAY, LAUNCH_DELAY, LIGHTWALLETD_DELAY},
    lightwalletd::{
        random_known_rpc_port_config, zebra_skip_lightwalletd_tests, LightWalletdTestDirExt,
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

fn restart_stop_at_height_for_network(network: Network, height: Height) -> Result<()> {
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
        Height(TINY_CHECKPOINT_TEST_HEIGHT.0 + 1),
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
            Height::MAX,
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

/// Failure log messages for any process, from the OS or shell.
///
/// These messages show that the child process has failed.
/// So when we see them in the logs, we make the test fail.
const PROCESS_FAILURE_MESSAGES: &[&str] = &[
    // Linux
    "Aborted",
    // macOS / BSDs
    "Abort trap",
    // TODO: add other OS or C library errors?
];

/// Failure log messages from Zebra.
///
/// These `zebrad` messages show that the `lightwalletd` integration test has failed.
/// So when we see them in the logs, we make the test fail.
const ZEBRA_FAILURE_MESSAGES: &[&str] = &[
    // Rust-specific panics
    "The application panicked",
    // RPC port errors
    "Unable to start RPC server",
    // TODO: disable if this actually happens during test zebrad shutdown
    "Stopping RPC endpoint",
    // Missing RPCs in zebrad logs (this log is from PR #3860)
    //
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "Received unrecognized RPC request",
    // RPC argument errors: parsing and data
    //
    // These logs are produced by jsonrpc_core inside Zebra,
    // but it doesn't log them yet.
    //
    // TODO: log these errors in Zebra, and check for them in the Zebra logs?
    "Invalid params",
    "Method not found",
];

/// Failure log messages from lightwalletd.
///
/// These `lightwalletd` messages show that the `lightwalletd` integration test has failed.
/// So when we see them in the logs, we make the test fail.
const LIGHTWALLETD_FAILURE_MESSAGES: &[&str] = &[
    // Go-specific panics
    "panic:",
    // Missing RPCs in lightwalletd logs
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "unable to issue RPC call",
    // RPC response errors: parsing and data
    //
    // jsonrpc_core error messages from Zebra,
    // received by lightwalletd and written to its logs
    "Invalid params",
    "Method not found",
    // Early termination
    //
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "Lightwalletd died with a Fatal error",
    // Go json package error messages:
    "json: cannot unmarshal",
    "into Go value of type",
    // lightwalletd custom RPC error messages from:
    // https://github.com/adityapk00/lightwalletd/blob/master/common/common.go
    "block requested is newer than latest block",
    "Cache add failed",
    "error decoding",
    "error marshaling",
    "error parsing JSON",
    "error reading JSON response",
    "error with",
    // We expect these errors when lightwalletd reaches the end of the zebrad cached state
    // "error requesting block: 0: Block not found",
    // "error zcashd getblock rpc",
    "received overlong message",
    "received unexpected height block",
    "Reorg exceeded max",
    "unable to issue RPC call",
    // Missing fields for each specific RPC
    //
    // get_block_chain_info
    //
    // invalid sapling height
    "Got sapling height 0",
    // missing BIP70 chain name, should be "main" or "test"
    " chain  ",
    // missing branchID, should be 8 hex digits
    " branchID \"",
    // get_block
    //
    // a block error other than "-8: Block not found"
    "error requesting block",
    // a missing block with an incorrect error code
    "Block not found",
    //
    // TODO: complete this list for each RPC with fields, if that RPC generates logs
    // get_info - doesn't generate logs
    // get_raw_transaction - might not generate logs
    // z_get_tree_state
    // get_address_txids
    // get_address_balance
    // get_address_utxos
];

/// Ignored failure logs for lightwalletd.
/// These regexes override the [`LIGHTWALLETD_FAILURE_MESSAGES`].
///
/// These `lightwalletd` messages look like failure messages, but they are actually ok.
/// So when we see them in the logs, we make the test continue.
const LIGHTWALLETD_IGNORE_MESSAGES: &[&str] = &[
    // Exceptions to lightwalletd custom RPC error messages:
    //
    // This log matches the "error with" RPC error message,
    // but we expect Zebra to start with an empty state.
    //
    // TODO: this exception should not be used for the cached state tests (#3511)
    r#"No Chain tip available yet","level":"warning","msg":"error with getblockchaininfo rpc, retrying"#,
];

/// Launch `zebrad` with an RPC port, and make sure `lightwalletd` works with Zebra.
///
/// This test only runs when the `ZEBRA_TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lightwalletd_integration() -> Result<()> {
    zebra_test::init();

    // Skip the test unless the user specifically asked for it
    if zebra_skip_lightwalletd_tests() {
        return Ok(());
    }

    // Launch zebrad

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = random_known_rpc_port_config()?;

    let zdir = testdir()?.with_config(&mut config)?;
    let mut zebrad = zdir
        .spawn_child(args!["start"])?
        .with_timeout(LAUNCH_DELAY)
        .with_failure_regex_iter(
            // TODO: replace with a function that returns the full list and correct return type
            ZEBRA_FAILURE_MESSAGES
                .iter()
                .chain(PROCESS_FAILURE_MESSAGES)
                .cloned(),
            NO_MATCHES_REGEX_ITER.iter().cloned(),
        );

    // Wait until `zebrad` has opened the RPC endpoint
    zebrad.expect_stdout_line_matches(
        format!("Opened RPC endpoint at {}", config.rpc.listen_addr.unwrap()).as_str(),
    )?;

    // Launch lightwalletd

    // Write a fake zcashd configuration that has the rpcbind and rpcport options set
    let ldir = testdir()?;
    let ldir = ldir.with_lightwalletd_config(config.rpc.listen_addr.unwrap())?;

    // Launch the lightwalletd process
    let result = ldir.spawn_lightwalletd_child(args![]);
    let (lightwalletd, zebrad) = zebrad.kill_on_error(result)?;
    let mut lightwalletd = lightwalletd
        .with_timeout(LIGHTWALLETD_DELAY)
        .with_failure_regex_iter(
            // TODO: replace with a function that returns the full list and correct return type
            LIGHTWALLETD_FAILURE_MESSAGES
                .iter()
                .chain(PROCESS_FAILURE_MESSAGES)
                .cloned(),
            // TODO: some exceptions do not apply to the cached state tests (#3511)
            LIGHTWALLETD_IGNORE_MESSAGES.iter().cloned(),
        );

    // Wait until `lightwalletd` has launched
    let result = lightwalletd.expect_stdout_line_matches("Starting gRPC server");
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // Check that `lightwalletd` is calling the expected Zebra RPCs

    // getblockchaininfo
    //
    // TODO: update branchID when we're using cached state (#3511)
    //       add "Waiting for zcashd height to reach Sapling activation height"
    let result = lightwalletd.expect_stdout_line_matches(
        "Got sapling height 419200 block height [0-9]+ chain main branchID 00000000",
    );
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    let result = lightwalletd.expect_stdout_line_matches("Found 0 blocks in cache");
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // getblock with the first Sapling block in Zebra's state
    //
    // zcash/lightwalletd calls getbestblockhash here, but
    // adityapk00/lightwalletd calls getblock
    //
    // The log also depends on what is in Zebra's state:
    //
    // # Empty Zebra State
    //
    // lightwalletd tries to download the Sapling activation block, but it's not in the state.
    //
    // Until the Sapling activation block has been downloaded, lightwalletd will log Zebra's RPC error:
    // "error requesting block: 0: Block not found"
    // We also get a similar log when lightwalletd reaches the end of Zebra's cache.
    //
    // # Cached Zebra State
    //
    // After the first successful getblock call, lightwalletd will log:
    // "Block hash changed, clearing mempool clients"
    // But we can't check for that, because it can come before or after the Ingestor log.
    //
    // TODO: expect Ingestor log when we're using cached state (#3511)
    //       "Ingestor adding block to cache"
    let result = lightwalletd.expect_stdout_line_matches(regex::escape(
        "Waiting for zcashd height to reach Sapling activation height (419200)",
    ));
    let (_, zebrad) = zebrad.kill_on_error(result)?;

    // (next RPC)
    //
    // TODO: add extra checks when we add new Zebra RPCs

    // Cleanup both processes
    let result = lightwalletd.kill();
    let (_, mut zebrad) = zebrad.kill_on_error(result)?;
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
        use color_eyre::eyre::eyre;
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

    // TODO: reuse code from https://github.com/ZcashFoundation/zebra/pull/4177/
    // to get the cached_state_path
    const CACHED_STATE_PATH_VAR: &str = "ZEBRA_CACHED_STATE_PATH";
    let cached_state_path = match env::var_os(CACHED_STATE_PATH_VAR) {
        Some(argument) => PathBuf::from(argument),
        None => {
            tracing::info!(
                "skipped send transactions using lightwalletd test, \
                 set the {CACHED_STATE_PATH_VAR:?} environment variable to run the test",
            );
            return Ok(());
        }
    };

    let network = Network::Mainnet;

    let (_zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc_without_initial_peers(network, cached_state_path)?;

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let client = reqwest::Client::new();
    let res = client
        .post(format!("http://{}", &zebra_rpc_address.to_string()))
        // Manually constructed request to avoid encoding it, for simplicity
        .body(r#"{"jsonrpc": "2.0", "method": "getblock", "params": ["1000000", 0], "id":123 }"#)
        .header("Content-Type", "application/json")
        .send()
        .await?
        .text()
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    const HEX_BLOCK: &str = "0400000077f36aa43aeba34a284bdb6aeabf55b7035fd490589cf498ea2d5101000000005addfb1cac535809e025523319a1e3fe65228e837a19bfab0a1f0a250ea0d5a1d5834caa7ab34a6b3fa8dbd6d2277643654e4f1ee5216ddd4a6e03efab4ab9484dbb7f5fa2d0011c000000000000000000000000000000000000470000000000000000008003f82cfd40050051da0866d2c8b7cd1b61d00b6a04731efc5b46d314d4b746c75baf930234e31b34de8c6263ee79fc360368d4da72d5a6bfcecba8d4befdcc368adb9f0f8c03daf6726325494754537b5356dc0e32fc301df4551b457b82d069498f5e3186bc56ef0dee10d7fa3c4d5387339e75a127ef40cd77d3455e4589f8f2123113276bc210484f9f837d11a2d4671902192bdcb2ac3e38c89ac7b0943dc7faff05c3af74a1a5d1ffb7dd44006a0a4070c690d677a10202f0ef2bd1c87537c4a10566f1e41ed251f7fd398535a4336fb651ba7f0db103df834386e9deb972f5827beb997b7a5ca6da287c3260f3dd8d73a811acf85a0188fb5b33074cfd56c206a6bb84f2129b9cf01e687d47e8d2567dc8551631344cb4a33815a6cd5673d736f76b7de64a217f6fb0126263f46f1fc0e9efa9220791fd3598c88f921413236739282f231db92a8a33a21a3f1e1a701214cd2802e2c91a35ab40976a04913dc0f257850f23ef1ef1142e3e917a9058d72b0a9574e93c32ddc0d29c4a27121a642ad5cb16775cc0a28bd07015f5a89c128d802f033cfccc5c8f652773c9e92df4d1672d733d8fac08d98383a1f186bbbba078318e5f7902e40239bf2e1a47f208a1de92ffffc42250d63ffd40ece6edd88723082ab865ce7b76bd9bb242ae777802453d55cbd72914fdca2d103c9d5b2fd2d9a4d08840d54e1f3a89032d07fb7e16233b3732e071232ae1a2943017a10435628a4fb96fa39b970206b59ae8f24e31edb769140a801d31b048b3b9aa46a0cc6ff17d091ecd31fa361aff4cccebc7d2caf8780645fb58ad4e9f9dbfed0407e90adf8506be6dc8f1b37c993d834e240a778a77261dd3d18e1c07c547edc6f7e0f5902e2e225e320e107f855096d6ae85d3ba630e4028f11ac79b37ceb61686b6bce0884308e9a8d3d636d1fd81377173be3e013c1625eaf019d3baad42f7bf25086581a56d8e120c6b72cc4a5a7203cebf50d4a8f50ca1af4f951f4a0ce20783844982416d4d46f575f88582717a3cc2b86a81e3d0fd9b9c0dfb79a88beec7123a5c5d9f978503c63189620a112cf0734b11f8e8372b0ebf7c95ea3fc98ef4bed98c11313e08cb19eeced3cf05ffedee3c706b4f0cd42fc763b6a4c9f1568f360c9bbe6ba03e4c7450365942efd5b7171329632836b6f09d3b1301882bf9df7db09df41a902ca0c985e8d23706b18b0572d2f1c4d3afa96189a07db874aa40cd0cb0e234280dc571e9f6d33bf8e6f4504fbd8b4a66b35dd0965c818c828619583da5a066a9eaebeec5f645f4e66805bd74c9810c9c29724712ca091ac569b59d1d3e7d0988d0964fa5fd7df0d0f09eaa1a71cd8df53763db07a786b7ca5adf0ff519b3dd8b1f48ea4e4975a96422dc43207094e291d30f34a1307c7b91ca8bd53ef5020a1763e85a5107e0f5c64213c13923540c5d0e751bae76a3b68ab028b4817324385a1e1b0c837dc2c7021e2534bf93150b0220958ea7e5d1057dd4b359a127d67ad98a91814583c193fb6a44e82f3df09cd0400838104c5831973fc74a812d35def1704dccbd95772198bca9ef4c16f6bac912d7fe99bcae3969f6bc523549db00b555713eef0a2655343df25652d434388642140d3f32d3278dd7e6e5cbfcca192982320f1d0f03bad5a3f4c80e1c69123137add41fa776e39c708c0781a0f74e44baeb968be1a110308198951370d400111079e0e40b0d25a6a29231765316059fb0f904fa117dcb7c434d4f458bf021e68f4a27058074748565149a7c01036f9a8feac2e38d5960f033cb0c9f6ebb3a3e89d3745fa99de1a74469f6497559259b321dce294d0f9577cffc7af80bd2c40b4a0879cae4fcd68f70b4cf0eb69f9caf1786196d5053f4e2781d1e53a9060400008085202f89010000000000000000000000000000000000000000000000000000000000000000ffffffff250340420f0fe4b883e5bda9e7a59ee4bb99e9b1bc104d696e656420627920676176696e313149b7525402249ccd1d000000001976a9149ef3382375369c5f3b5e41ed4fc48fa8549aac3988ac405973070000000017a91447185bab99466b849d774faa2edfa983bd3774148700228c200000000000000000000000000000000400008085202f8901a0960e24269869fe0cb8e81ff6188fdd675d3846b1218e42232cde314b7a9316000000006a4730440220691d6d926a417faf8bd241e02be6ac512c6ab263cf443e1aa64b4cfd553a802702204ac1a0d97030a689427fb272023a6b4477b7f23ca7be4e099430ffde78be1ee70121023c98e6ddf69091291f1eec341a1f5f8364efab0cb2ec5631ab5c2c6822ee4b23ffffffff02111ef205000000001976a91432e79f48ae5aef9c7b60801e1b55428e168eb15c88ac5db90300000000001976a914f45dab2d81f6ea3e1e6050326d2fc02885d7752788ac000000000000000000000000000000000000000400008085202f8901e166bf18d23f6a581391d33472db0202a0f362565fd741b177c1e542d871658a000000006a473044022009d66f79793012ef1201f2a39405979b5c99f60c0eda8fafc7733a463c8d74670220243c4891ca5b6924291513f6bb265ddce132789212de54acb8da31fb7fd2d788012102abeb1ac05121f69df5715a7c4980be3c570c56b4a58fee04651eddb88742701cfeffffff021b9a1755530000001976a914d97325c00fc728bbd0a6ecd84ac6472414d8610588acb2945d81020000001976a9140098a1ad96d580e8ecda1273051385cfe033935988ac35420f0068420f0000000000000000000000000400008085202f89029430018466c24c99f84657a5f000246b9b6cfef54da53c279c491d31e9657b2e5d0000006a473044022070bead6f51c69c09f12dd6921ca84c6fcf649b4ffc74bc58ec186efda1754835022004f71f95383087ea1d286a4d29f91d687194deb8ed3f1e778e05fb8f6512883e012103c21cf6f0ca752e549bb97ca6605ddafa2260b2767e06ebeaa1816e07710e2efdffffffffe16a914d4cd8adb965300ea7e995cfd32bf8ca56821eda63cf419a52284149a9000000006a4730440220612ca09675aa9b57ed35ae232c8fc431dbdb793b0bb866dfcf206b8f2ffa29d902203e71ce227184eaef66632c6f4b079408407ee763445ae57914f809c1874ac4fc0121037e609ec336382fc08dfc98a6568a82eb5b60b4ec324d79bfe12d4ca3949d34bbffffffff014893b5b5040000001976a91457787bcbe709bf4511a79eb356c1772d7a734d5388ac0000000067420f0000000000000000000000000400008085202f890173f77790296444883bd612c94a010d42e1f55f57c11bdf5e36de21b3a3c56936010000006a47304402203c1f53208a3bee1cc69a653da49a9cf13bb4dbddbd9b477300339fab5c68939f02201e69969a553e82dd185d3bf316ceb65240a903d09968e9c82a2d1a33549400cb0121035148109f816ec889ead8066829ad8132f06caaf98db6fdb745452489544336bdffffffff0240420f00000000001976a914ba4ff7e3ee85d17077ec10922fcf3a7cec1d55bb88acbe325207000000001976a91454b42d78507235610da54a3c7a55e942830e41d788ac00000000ff64cd1d00000000000000000000000400008085202f890150474bef0b34199d220b7d7e75a288bcf0cbef3022e287be79aaeae256c4b2ea010000006a473044022004fc4572a019a144d9bcdf795498401d65a453b1031bdf6293b6ea9085f2819802200ad4315170aaa80589254fc66cb5b5280ad9c385d3bb2a2de23d31788555ba7301210224c5d6e287b4f193fe48e874f0ce3ce7e0659c68effc6773502ee726b68a6a56ffffffff0232a00600000000001976a914200f6fae96ed4bebbaf115ddb10228fceeb6ccf588acaeb41c05000000001976a9144554bb3b9c3653c588d9136cdcfe254260be742a88ac0000000068420f000000000000000000000000";
    assert!(
        res.contains(HEX_BLOCK),
        "response did not contain the desired block: {}",
        res
    );

    Ok(())
}

/// Spawns a zebrad instance to interact with lightwalletd, but without an internet connection.
///
/// This prevents it from downloading blocks. Instead, the `zebra_directory` parameter allows
/// providing an initial state to the zebrad instance.
fn spawn_zebrad_for_rpc_without_initial_peers(
    network: Network,
    zebra_directory: PathBuf,
) -> Result<(TestChild<PathBuf>, SocketAddr)> {
    let mut config = random_known_rpc_port_config()
        .expect("Failed to create a config file with a known RPC listener port");

    config.state.ephemeral = false;
    config.network.initial_mainnet_peers = HashSet::new();
    config.network.initial_testnet_peers = HashSet::new();
    config.network.network = network;

    let mut zebrad = zebra_directory
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(Duration::from_secs(60 * 60))
        .bypass_test_capture(true);

    let rpc_address = config.rpc.listen_addr.unwrap();

    zebrad.expect_stdout_line_matches(&format!("Opened RPC endpoint at {}", rpc_address))?;

    Ok((zebrad, rpc_address))
}
