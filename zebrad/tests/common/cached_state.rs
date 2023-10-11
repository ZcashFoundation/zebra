//! Utility functions for tests that used cached Zebra state.
//!
//! Note: we allow dead code in this module, because it is mainly used by the gRPC tests,
//! which are optional.

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use semver::Version;
use tower::{util::BoxService, Service};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{ChainTipChange, LatestChainTip, MAX_BLOCK_REORG_HEIGHT};
use zebra_test::command::TestChild;

use crate::common::{
    launch::spawn_zebrad_for_rpc,
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    test_type::TestType,
};

/// The environmental variable that holds the path to a directory containing a cached Zebra state.
pub const ZEBRA_CACHED_STATE_DIR: &str = "ZEBRA_CACHED_STATE_DIR";

/// In integration tests, the interval between database format checks for newly added blocks.
///
/// This should be short enough that format bugs cause CI test failures,
/// but long enough that it doesn't impact performance.
pub const DATABASE_FORMAT_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Is the current state version upgrade longer than the typical CI sync time?
///
/// If is is set to `false`, but the state upgrades finish after zebrad is synced.
/// incomplete upgrades will be written to the cached state.
///
/// If this is set to `true`, but the state upgrades finish before zebrad is synced,
/// some tests will hang.
pub const DATABASE_FORMAT_UPGRADE_IS_LONG: bool = false;

/// Type alias for a boxed state service.
pub type BoxStateService =
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>;

/// Waits for the startup logs generated by the cached state version checks.
/// Returns the state version log message.
///
/// This function should be called immediately after launching `zebrad`.
#[tracing::instrument(skip(zebrad))]
pub fn wait_for_state_version_message<T>(zebrad: &mut TestChild<T>) -> Result<String> {
    tracing::info!(
        zebrad = ?zebrad.cmd,
        "launched zebrad, waiting for zebrad to open the state database..."
    );

    // Zebra logs one of these lines on startup, depending on the disk and running formats.
    zebrad.expect_stdout_line_matches(
        "(creating new database with the current format)|\
         (trying to open older database format)|\
         (trying to open newer database format)|\
         (trying to open current database format)",
    )
}

/// Waits for the `required_version` state upgrade to complete, if needed.
///
/// This function should be called with the output of [`wait_for_state_version_message()`].
#[tracing::instrument(skip(zebrad))]
pub fn wait_for_state_version_upgrade<T>(
    zebrad: &mut TestChild<T>,
    state_version_message: &str,
    required_version: Version,
) -> Result<()> {
    if state_version_message.contains("launching upgrade task") {
        tracing::info!(
            zebrad = ?zebrad.cmd,
            %state_version_message,
            %required_version,
            "waiting for zebrad state upgrade..."
        );

        let upgrade_message = zebrad.expect_stdout_line_matches(&format!(
            "marked database format as upgraded.*format_upgrade_version.*=.*{required_version}"
        ))?;

        tracing::info!(
            zebrad = ?zebrad.cmd,
            %state_version_message,
            %required_version,
            %upgrade_message,
            "zebrad state has been upgraded"
        );
    }

    Ok(())
}

/// Starts a state service using the provided `cache_dir` as the directory with the chain state.
#[tracing::instrument(skip(cache_dir))]
pub async fn start_state_service_with_cache_dir(
    network: Network,
    cache_dir: impl Into<PathBuf>,
) -> Result<(
    BoxStateService,
    impl Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
    LatestChainTip,
    ChainTipChange,
)> {
    let config = zebra_state::Config {
        cache_dir: cache_dir.into(),
        ..zebra_state::Config::default()
    };

    // These tests don't need UTXOs to be verified efficiently, because they use cached states.
    Ok(zebra_state::init(config, network, Height::MAX, 0))
}

/// Loads the chain tip height from the state stored in a specified directory.
#[tracing::instrument]
pub async fn load_tip_height_from_state_directory(
    network: Network,
    state_path: &Path,
) -> Result<block::Height> {
    let (_state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
        start_state_service_with_cache_dir(network, state_path).await?;

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    Ok(chain_tip_height)
}

/// Accepts a network, test_type, test_name, and num_blocks (how many blocks past the finalized tip to try getting)
///
/// Syncs zebra until the tip, gets some blocks near the tip, via getblock rpc calls,
/// shuts down zebra, and gets the finalized tip height of the updated cached state.
///
/// Returns retrieved and deserialized blocks that are above the finalized tip height of the cached state.
///
/// ## Panics
///
/// If the provided `test_type` doesn't need an rpc server and cached state, or if `max_num_blocks` is 0
pub async fn get_future_blocks(
    network: Network,
    test_type: TestType,
    test_name: &str,
    max_num_blocks: u32,
) -> Result<Vec<Block>> {
    let blocks: Vec<Block> = get_raw_future_blocks(network, test_type, test_name, max_num_blocks)
        .await?
        .into_iter()
        .map(hex::decode)
        .map(|block_bytes| {
            block_bytes
                .expect("getblock rpc calls in get_raw_future_blocks should return valid hexdata")
                .zcash_deserialize_into()
                .expect("decoded hex data from getblock rpc calls should deserialize into blocks")
        })
        .collect();

    Ok(blocks)
}

/// Accepts a network, test_type, test_name, and num_blocks (how many blocks past the finalized tip to try getting)
///
/// Syncs zebra until the tip, gets some blocks near the tip, via getblock rpc calls,
/// shuts down zebra, and gets the finalized tip height of the updated cached state.
///
/// Returns hexdata of retrieved blocks that are above the finalized tip height of the cached state.
///
/// ## Panics
///
/// If the provided `test_type` doesn't need an rpc server and cached state, or if `max_num_blocks` is 0
pub async fn get_raw_future_blocks(
    network: Network,
    test_type: TestType,
    test_name: &str,
    max_num_blocks: u32,
) -> Result<Vec<String>> {
    assert!(max_num_blocks > 0);

    let max_num_blocks = max_num_blocks.min(MAX_BLOCK_REORG_HEIGHT);
    let mut raw_blocks = Vec::with_capacity(max_num_blocks as usize);

    assert!(
        test_type.needs_zebra_cached_state() && test_type.needs_zebra_rpc_server(),
        "get_raw_future_blocks needs zebra cached state and rpc server"
    );

    let should_sync = true;
    let (zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
            .ok_or_else(|| eyre!("get_raw_future_blocks requires a cached state"))?;
    let rpc_address = zebra_rpc_address.expect("test type must have RPC port");

    let mut zebrad = check_sync_logs_until(
        zebrad,
        network,
        SYNC_FINISHED_REGEX,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
    )?;

    // Create an http client
    let rpc_client = RpcRequestClient::new(rpc_address);

    let blockchain_info: serde_json::Value = serde_json::from_str(
        &rpc_client
            .text_from_call("getblockchaininfo", "[]".to_string())
            .await?,
    )?;

    let tip_height: u32 = blockchain_info["result"]["blocks"]
        .as_u64()
        .expect("unexpected block height: doesn't fit in u64")
        .try_into()
        .expect("unexpected block height: doesn't fit in u32");

    let estimated_finalized_tip_height = tip_height - MAX_BLOCK_REORG_HEIGHT;

    tracing::info!(
        ?tip_height,
        ?estimated_finalized_tip_height,
        "got tip height from blockchaininfo",
    );

    for block_height in (0..max_num_blocks).map(|idx| idx + estimated_finalized_tip_height) {
        let raw_block: serde_json::Value = serde_json::from_str(
            &rpc_client
                .text_from_call("getblock", format!(r#"["{block_height}", 0]"#))
                .await?,
        )?;

        raw_blocks.push((
            block_height,
            raw_block["result"]
                .as_str()
                .expect("unexpected getblock result: not a string")
                .to_string(),
        ));
    }

    zebrad.kill(true)?;

    // Sleep for a few seconds to make sure zebrad releases lock on cached state directory
    std::thread::sleep(Duration::from_secs(3));

    let zebrad_state_path = test_type
        .zebrad_state_path(test_name)
        .expect("already checked that there is a cached state path");

    let Height(finalized_tip_height) =
        load_tip_height_from_state_directory(network, zebrad_state_path.as_ref()).await?;

    tracing::info!(
        ?finalized_tip_height,
        non_finalized_tip_height = ?tip_height,
        ?estimated_finalized_tip_height,
        "got finalized tip height from state directory"
    );

    let raw_future_blocks = raw_blocks
        .into_iter()
        .filter_map(|(height, raw_block)| height.gt(&finalized_tip_height).then_some(raw_block))
        .collect();

    Ok(raw_future_blocks)
}
