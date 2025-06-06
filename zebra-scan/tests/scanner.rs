//! `zebra-scanner` binary tests.

use std::path::Path;

#[cfg(not(target_os = "windows"))]
use std::io::Write;

use tempfile::TempDir;

use zebra_test::{
    args,
    command::{Arguments, TestDirExt},
    prelude::*,
};

#[cfg(not(target_os = "windows"))]
use zebra_grpc::scanner::{scanner_client::ScannerClient, Empty};

mod scan_task_commands;

/// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
#[cfg(not(target_os = "windows"))]
const ZECPAGES_SAPLING_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

/// Test the scanner binary with the `--help` flag.
#[test]
fn scanner_help() -> eyre::Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;

    let child = testdir.spawn_scanner_child(args!["--help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_line_contains("USAGE:")?;

    Ok(())
}

/// Test that the scanner binary starts the scan task.
///
/// This test creates a new zebrad cache dir by using a `zebrad` binary specified in the `CARGO_BIN_EXE_zebrad` env variable.
///
/// To run it locally, one way is:
///
/// ```
/// cargo test scan_binary_starts -- --nocapture
/// ```
#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn scan_binary_starts() -> Result<()> {
    use std::time::Duration;

    use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
    use zebra_test::net::random_known_port;

    let _init_guard = zebra_test::init();

    // Create a directory to dump test data into it.
    let test_dir = testdir()?;

    // Create a config for zebrad with a cache directory inside the test directory.
    let mut config = zebrad::config::ZebradConfig::default();
    config.state.cache_dir = test_dir.path().join("zebra").to_path_buf();

    let indexer_rpc_listen_addr = &format!("127.0.0.1:{}", random_known_port());
    config.rpc.indexer_listen_addr = indexer_rpc_listen_addr.parse().ok();
    config.network.initial_mainnet_peers = Default::default();
    let config_file = test_dir.path().join("zebrad.toml");
    std::fs::File::create(config_file.clone())?.write_all(toml::to_string(&config)?.as_bytes())?;

    // Start the node just to make sure a cache is created.
    let mut zebrad =
        test_dir.spawn_zebrad_child(args!["-c", config_file.clone().to_str().unwrap(), "start"])?;

    zebrad.expect_stdout_line_matches("Opened Zebra state cache at .*")?;

    zebrad.expect_stdout_line_matches(OPENED_RPC_ENDPOINT_MSG)?;

    // Create a new directory for the scanner
    let test_dir = testdir()?;

    // Create arguments for the scanner
    let key = serde_json::json!({"key": ZECPAGES_SAPLING_VIEWING_KEY}).to_string();
    let listen_addr = "127.0.0.1:8231";
    let rpc_listen_addr = "127.0.0.1:8232";
    let zebrad_cache_dir = config.state.cache_dir.clone();
    let scanning_cache_dir = test_dir.path().join("scanner").to_path_buf();
    let args = args![
        "--zebrad-cache-dir",
        zebrad_cache_dir.to_str().unwrap(),
        "--scanning-cache-dir",
        scanning_cache_dir.to_str().unwrap(),
        "--sapling-keys-to-scan",
        key,
        "--listen-addr",
        listen_addr,
        "--zebra-rpc-listen-addr",
        rpc_listen_addr,
        "--zebra-indexer-rpc-listen-addr",
        indexer_rpc_listen_addr
    ];

    // Start the scaner using another test directory.
    let mut zebra_scanner = test_dir.spawn_scanner_child(args)?;

    // Check scanner was started.
    zebra_scanner.expect_stdout_line_matches("loaded Zebra scanner cache")?;

    // Wait for the scanner's gRPC server to start up
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check if the grpc server is up
    let mut client = ScannerClient::connect(format!("http://{listen_addr}")).await?;
    let request = tonic::Request::new(Empty {});
    client.get_info(request).await?;

    // Look for 2 scanner notices indicating we are below sapling activation.
    zebra_scanner.expect_stdout_line_matches("scanner is waiting for Sapling activation. Current tip: [0-9]{1,4}, Sapling activation: 419200")?;
    zebra_scanner.expect_stdout_line_matches("scanner is waiting for Sapling activation. Current tip: [0-9]{1,4}, Sapling activation: 419200")?;

    // Kill the scanner.
    zebra_scanner.kill(true)?;

    // Check that scan task started and the scanning is done.
    let output = zebra_scanner.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;
    output.assert_failure()?;

    zebrad.kill(false)?;
    let output = zebrad.wait_with_output()?;
    output.assert_was_killed()?;

    Ok(())
}

/// Test that the scanner can continue scanning where it was left when zebrad restarts.
///
/// Needs a cache state close to the tip. A possible way to run it locally is:
///
/// export ZEBRA_CACHE_DIR="/path/to/zebra/state"
/// cargo test scan_start_where_left -- --ignored --nocapture
///
/// The test will run zebrad with a key to scan, scan the first few blocks after sapling and then stops.
/// Then it will restart zebrad and check that it resumes scanning where it was left.
#[ignore]
#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn scan_start_where_left() -> Result<()> {
    let _init_guard = zebra_test::init();

    let Ok(zebrad_cache_dir) = std::env::var("ZEBRA_CACHE_DIR") else {
        tracing::warn!("env var ZEBRA_CACHE_DIR is not set, skipping test");
        return Ok(());
    };

    if !Path::new(&zebrad_cache_dir).join("state").is_dir() {
        tracing::warn!("cache dir does not contain cached state, skipping test");
        return Ok(());
    }

    // Logs the network as zebrad would as part of the metadata when starting up.
    // This is currently needed for the 'Check startup logs' step in CI to pass.
    let mainnet = zebra_chain::parameters::Network::Mainnet;
    tracing::info!("Zcash network: {mainnet}");

    let scanning_cache_dir = testdir()?.path().join("scanner").to_path_buf();

    // Create arguments for the scanner
    let key = serde_json::json!({"key": ZECPAGES_SAPLING_VIEWING_KEY}).to_string();
    let listen_addr = "127.0.0.1:18231";
    let rpc_listen_addr = "127.0.0.1:18232";
    let args = args![
        "--zebrad-cache-dir",
        zebrad_cache_dir,
        "--scanning-cache-dir",
        scanning_cache_dir.to_str().unwrap(),
        "--sapling-keys-to-scan",
        key,
        "--listen-addr",
        listen_addr,
        "--zebra-rpc-listen-addr",
        rpc_listen_addr
    ];

    // Start the scaner using another test directory.
    let mut zebra_scanner: TestChild<TempDir> = testdir()?.spawn_scanner_child(args.clone())?;

    // Check scanner was started.
    zebra_scanner.expect_stdout_line_matches("loaded Zebra scanner cache")?;

    // The first time
    zebra_scanner.expect_stdout_line_matches(
        r"Scanning the blockchain for key 0, started at block 419200, now at block 420000",
    )?;

    // Make sure scanner scans a few blocks.
    zebra_scanner.expect_stdout_line_matches(
        r"Scanning the blockchain for key 0, started at block 419200, now at block 430000",
    )?;
    zebra_scanner.expect_stdout_line_matches(
        r"Scanning the blockchain for key 0, started at block 419200, now at block 440000",
    )?;

    // Kill the scanner.
    zebra_scanner.kill(true)?;

    // Check that scan task started and the first scanning is done.
    let output = zebra_scanner.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;
    output.assert_failure()?;

    // Start the node again with the same arguments.
    let mut zebra_scanner: TestChild<TempDir> = testdir()?.spawn_scanner_child(args)?;

    // Resuming message.
    zebra_scanner.expect_stdout_line_matches(
        "Last scanned height for key number 0 is 439000, resuming at 439001",
    )?;
    zebra_scanner.expect_stdout_line_matches("loaded Zebra scanner cache")?;

    // Start scanning where it was left.
    zebra_scanner.expect_stdout_line_matches(
        r"Scanning the blockchain for key 0, started at block 439001, now at block 440000",
    )?;
    zebra_scanner.expect_stdout_line_matches(
        r"Scanning the blockchain for key 0, started at block 439001, now at block 450000",
    )?;

    // Kill the scanner.
    zebra_scanner.kill(true)?;

    // Check that scan task started and the second scanning is done.
    let output = zebra_scanner.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;
    output.assert_failure()?;

    Ok(())
}

/// Tests successful:
/// - Registration of a new key,
/// - Subscription to scan results of new key, and
/// - Deletion of keys
/// in the scan task
/// See [`common::shielded_scan::scan_task_commands`] for more information.
///
/// Example of how to run the scan_task_commands test locally:
///
/// ```console
/// RUST_LOG=info ZEBRA_CACHE_DIR=/path/to/zebra/state cargo test scan_task_commands -- --include-ignored --nocapture
/// ```
#[tokio::test]
#[ignore]
async fn scan_task_commands() -> Result<()> {
    scan_task_commands::run().await
}

/// Create a temporary directory for testing `zebra-scanner`.
pub fn testdir() -> eyre::Result<TempDir> {
    tempfile::Builder::new()
        .prefix("zebra_scanner_tests")
        .tempdir()
        .map_err(Into::into)
}

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `zebra-scanner`.
pub trait ScannerTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `zebra-scanner` with `args` as a child process in this test directory.
    fn spawn_scanner_child(self, args: Arguments) -> Result<TestChild<Self>>;

    /// Spawn `zebrad` with `args` as a child process in this test directory.
    fn spawn_zebrad_child(self, args: Arguments) -> Result<TestChild<Self>>;
}

impl<T> ScannerTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    fn spawn_scanner_child(self, extra_args: Arguments) -> Result<TestChild<Self>> {
        let mut args = Arguments::new();
        args.merge_with(extra_args);
        self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebra-scanner"), args)
    }
    fn spawn_zebrad_child(self, extra_args: Arguments) -> Result<TestChild<Self>> {
        let mut args = Arguments::new();
        args.merge_with(extra_args);
        self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad-for-scanner"), args)
    }
}
