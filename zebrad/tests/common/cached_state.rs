//! Utility functions for tests that used cached Zebra state.
//!
//! Note: we allow dead code in this module, because it is mainly used by the gRPC tests,
//! which are optional.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tempfile::TempDir;
use tokio::fs;
use tower::{util::BoxService, Service};

use zebra_chain::block::Block;
use zebra_chain::serialization::ZcashDeserializeInto;
use zebra_chain::{
    block::{self, Height},
    chain_tip::ChainTip,
    parameters::Network,
};
use zebra_state::{ChainTipChange, LatestChainTip};

use crate::common::config::testdir;
use crate::common::rpc_client::RPCRequestClient;

use zebra_state::MAX_BLOCK_REORG_HEIGHT;

use crate::common::{
    launch::spawn_zebrad_for_rpc,
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    test_type::TestType,
};

/// Path to a directory containing a cached Zebra state.
pub const ZEBRA_CACHED_STATE_DIR: &str = "ZEBRA_CACHED_STATE_DIR";

/// Type alias for a boxed state service.
pub type BoxStateService =
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>;

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

/// Recursively copy a chain state database directory into a new temporary directory.
pub async fn copy_state_directory(network: Network, source: impl AsRef<Path>) -> Result<TempDir> {
    // Copy the database files for this state and network, excluding testnet and other state versions
    let source = source.as_ref();
    let state_config = zebra_state::Config {
        cache_dir: source.into(),
        ..Default::default()
    };
    let source_net_dir = state_config.db_path(network);
    let source_net_dir = source_net_dir.as_path();
    let state_suffix = source_net_dir
        .strip_prefix(source)
        .expect("db_path() is a subdirectory");

    let destination = testdir()?;
    let destination_net_dir = destination.path().join(state_suffix);

    tracing::info!(
        ?source,
        ?source_net_dir,
        ?state_suffix,
        ?destination,
        ?destination_net_dir,
        "copying cached state files (this may take some time)...",
    );

    let mut remaining_directories = vec![PathBuf::from(source_net_dir)];

    while let Some(directory) = remaining_directories.pop() {
        let sub_directories =
            copy_directory(&directory, source_net_dir, destination_net_dir.as_ref()).await?;

        remaining_directories.extend(sub_directories);
    }

    Ok(destination)
}

/// Copy the contents of a directory, and return the sub-directories it contains.
///
/// Copies all files from the `directory` into the destination specified by the concatenation of
/// the `base_destination_path` and `directory` stripped of its `prefix`.
#[tracing::instrument]
async fn copy_directory(
    directory: &Path,
    prefix: &Path,
    base_destination_path: &Path,
) -> Result<Vec<PathBuf>> {
    let mut sub_directories = Vec::new();
    let mut entries = fs::read_dir(directory).await?;

    let destination =
        base_destination_path.join(directory.strip_prefix(prefix).expect("Invalid path prefix"));

    fs::create_dir_all(&destination).await?;

    while let Some(entry) = entries.next_entry().await? {
        let entry_path = entry.path();
        let file_type = entry.file_type().await?;

        if file_type.is_file() {
            let file_name = entry_path.file_name().expect("Missing file name");
            let destination_path = destination.join(file_name);

            fs::copy(&entry_path, destination_path).await?;
        } else if file_type.is_dir() {
            sub_directories.push(entry_path);
        } else if file_type.is_symlink() {
            unimplemented!("Symbolic link support is currently not necessary");
        } else {
            panic!("Unknown file type");
        }
    }

    Ok(sub_directories)
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
    let rpc_client = RPCRequestClient::new(rpc_address);

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
