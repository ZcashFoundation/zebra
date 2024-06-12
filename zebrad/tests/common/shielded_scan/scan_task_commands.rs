//! Test registering keys, subscribing to their results, and deleting keys in the scan task while zebrad is running.
//!
//! This test requires a cached chain state that is partially synchronized past the
//! Sapling activation height and [`REQUIRED_MIN_TIP_HEIGHT`]
//!
//! export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! cargo test scan_task_commands --features="shielded-scan" -- --ignored --nocapture

use std::{fs, time::Duration};

use color_eyre::{eyre::eyre, Result};

use tokio::sync::mpsc::error::TryRecvError;
use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
};

use zebra_scan::{
    service::ScanTask,
    storage::{db::SCANNER_DATABASE_KIND, Storage},
    tests::ZECPAGES_SAPLING_VIEWING_KEY,
};

use crate::common::{
    cached_state::start_state_service_with_cache_dir, launch::can_spawn_zebrad_for_test_type,
    test_type::TestType,
};

/// The minimum required tip height for the cached state in this test.
const REQUIRED_MIN_TIP_HEIGHT: Height = Height(1_000_000);

/// How long this test waits for a result before failing.
/// Should be long enough for ScanTask to start and scan ~500 blocks
const WAIT_FOR_RESULTS_DURATION: Duration = Duration::from_secs(60);

/// A block height where a scan result can be found with the [`ZECPAGES_SAPLING_VIEWING_KEY`]
const EXPECTED_RESULT_HEIGHT: Height = Height(780_532);

/// Initialize Zebra's state service with a cached state, then:
/// - Start the scan task,
/// - Add a new key,
/// - Subscribe to results for that key,
/// - Check that the scanner sends an expected result,
/// - Remove the key and,
/// - Check that the results channel is disconnected
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_type = TestType::UpdateZebraCachedStateNoRpc;
    let test_name = "scan_task_commands";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_test_type(test_name, test_type, true) {
        return Ok(());
    }

    tracing::info!(
        ?network,
        ?test_type,
        "running scan_subscribe_results test using zebra state service",
    );

    let zebrad_state_path = test_type
        .zebrad_state_path(test_name)
        .expect("already checked that there is a cached state path");

    let mut scan_config = zebra_scan::Config::default();
    scan_config
        .db_config_mut()
        .cache_dir
        .clone_from(&zebrad_state_path);

    // Logs the network as zebrad would as part of the metadata when starting up.
    // This is currently needed for the 'Check startup logs' step in CI to pass.
    tracing::info!("Zcash network: {network}");

    // Remove the scan directory before starting.
    let scan_db_path = zebrad_state_path.join(SCANNER_DATABASE_KIND);
    fs::remove_dir_all(std::path::Path::new(&scan_db_path)).ok();

    let (_state_service, _read_state_service, latest_chain_tip, chain_tip_change) =
        start_state_service_with_cache_dir(&network, zebrad_state_path.clone()).await?;

    let state_config = zebra_state::Config {
        cache_dir: zebrad_state_path.clone(),
        ..zebra_state::Config::default()
    };
    let (read_state, _db, _) = zebra_state::init_read_only(state_config, &network);

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    let sapling_activation_height = NetworkUpgrade::Sapling
        .activation_height(&network)
        .expect("there should be an activation height for Mainnet");

    assert!(
        sapling_activation_height < REQUIRED_MIN_TIP_HEIGHT,
        "minimum tip height should be above sapling activation height"
    );

    assert!(
        REQUIRED_MIN_TIP_HEIGHT < chain_tip_height,
        "chain tip height must be above required minimum tip height"
    );

    tracing::info!("opened state service with valid chain tip height, starting scan task",);

    // Create an ephemeral `Storage` instance
    let storage = Storage::new(&scan_config, &network, false);
    let mut scan_task = ScanTask::spawn(storage, read_state, chain_tip_change);

    tracing::info!("started scan task, sending register/subscribe keys messages with zecpages key to start scanning for a new key",);

    let keys = [ZECPAGES_SAPLING_VIEWING_KEY.to_string()];
    scan_task.register_keys(
        keys.iter()
            .cloned()
            .map(|key| (key, Some(EXPECTED_RESULT_HEIGHT.0)))
            .collect(),
    )?;

    let mut result_receiver = scan_task
        .subscribe(keys.iter().cloned().collect())
        .expect("should send subscribe message successfully")
        .await
        .expect("should receive response successfully");

    // Wait for the scanner to send a result in the channel
    let result = tokio::time::timeout(WAIT_FOR_RESULTS_DURATION, result_receiver.recv()).await?;

    tracing::info!(?result, "received a result from the channel");

    let result = result.expect("there should be some scan result");

    assert_eq!(
        EXPECTED_RESULT_HEIGHT, result.height,
        "result height should match expected height for hard-coded key"
    );

    scan_task.remove_keys(keys.to_vec())?;

    // Wait for scan task to drop results sender
    tokio::time::sleep(WAIT_FOR_RESULTS_DURATION).await;

    loop {
        match result_receiver.try_recv() {
            // Empty any messages in the buffer
            Ok(_) => continue,

            Err(recv_error) => {
                assert_eq!(
                    recv_error,
                    TryRecvError::Disconnected,
                    "any result senders should have been dropped"
                );

                break;
            }
        }
    }

    Ok(())
}
