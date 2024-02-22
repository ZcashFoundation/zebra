//! Test registering and subscribing to the results for a new key in the scan task while zebrad is running.
//!
//! This test requires a cached chain state that is partially synchronized past the
//! Sapling activation height and [`REQUIRED_MIN_TIP_HEIGHT`]
//!
//! export ZEBRA_CACHED_STATE_DIR="/path/to/zebra/state"
//! cargo test scan_subscribe_results --features="shielded-scan" -- --ignored --nocapture

use std::time::Duration;

use color_eyre::{eyre::eyre, Result};

use tower::ServiceBuilder;
use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
};

use zebra_scan::{service::ScanTask, storage::Storage, tests::ZECPAGES_SAPLING_VIEWING_KEY};

use crate::common::{
    cached_state::start_state_service_with_cache_dir, launch::can_spawn_zebrad_for_test_type,
    test_type::TestType,
};

/// The minimum required tip height for the cached state in this test.
const REQUIRED_MIN_TIP_HEIGHT: Height = Height(1_000_000);

/// How long this test waits for a result before failing.
const WAIT_FOR_RESULTS_DURATION: Duration = Duration::from_secs(30 * 60);

/// Initialize Zebra's state service with a cached state, add a new key to the scan task, and
/// check that it stores results for the new key without errors.
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    let test_type = TestType::UpdateZebraCachedStateNoRpc;
    let test_name = "scan_subscribe_results";
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

    let (state_service, _read_state_service, latest_chain_tip, chain_tip_change) =
        start_state_service_with_cache_dir(network, zebrad_state_path).await?;

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    let sapling_activation_height = NetworkUpgrade::Sapling
        .activation_height(network)
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

    let state = ServiceBuilder::new().buffer(10).service(state_service);

    // Create an ephemeral `Storage` instance
    let storage = Storage::new(&zebra_scan::Config::ephemeral(), network, false);
    let mut scan_task = ScanTask::spawn(storage, state, chain_tip_change);

    tracing::info!("started scan task, sending register/subscribe keys messages with zecpages key to start scanning for a new key",);

    let keys = [ZECPAGES_SAPLING_VIEWING_KEY.to_string()];
    scan_task.register_keys(
        keys.iter()
            .cloned()
            .map(|key| (key, Some(780_000)))
            .collect(),
    )?;

    let mut result_receiver = scan_task
        .subscribe(keys.into_iter().collect())
        .await
        .expect("should send and receive message successfully");

    // Wait for the scanner to send a result in the channel
    let result = tokio::time::timeout(WAIT_FOR_RESULTS_DURATION, result_receiver.recv()).await?;

    tracing::info!(?result, "received a result from the channel");

    Ok(())
}
