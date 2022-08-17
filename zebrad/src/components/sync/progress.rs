//! Progress tracking for blockchain syncing.

use std::{ops::Add, time::Duration};

use chrono::Utc;
use num_integer::div_ceil;

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    fmt::humantime_seconds,
    parameters::{Network, NetworkUpgrade, POST_BLOSSOM_POW_TARGET_SPACING},
};
use zebra_consensus::CheckpointList;

use crate::components::sync::SyncStatus;

/// The amount of time between progress logs.
const LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The number of blocks we consider to be close to the tip.
///
/// Most chain forks are 1-7 blocks long.
const MAX_CLOSE_TO_TIP_BLOCKS: i32 = 1;

/// Skip slow sync warnings when we are this close to the tip.
///
/// In testing, we've seen warnings around 30 blocks.
///
/// TODO: replace with `MAX_CLOSE_TO_TIP_BLOCKS` after fixing slow syncing near tip (#3375)
const MIN_SYNC_WARNING_BLOCKS: i32 = 60;

/// The number of fractional digits in sync percentages.
const SYNC_PERCENT_FRAC_DIGITS: usize = 3;

/// The minimum number of extra blocks mined between updating a checkpoint list,
/// and running an automated test that depends on that list.
///
/// Makes sure that the block finalization code always runs in sync tests,
/// even if the miner or test node clock is wrong by a few minutes.
///
/// This is an estimate based on the time it takes to:
/// - get the tip height from `zcashd`,
/// - run `zebra-checkpoints` to update the checkpoint list,
/// - submit a pull request, and
/// - run a CI test that logs progress based on the new checkpoint height.
///
/// We might add tests that sync from a cached tip state,
/// so we only allow a few extra blocks here.
const MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE: i32 = 10;

/// Logs Zebra's estimated progress towards the chain tip every minute or so.
///
/// TODO:
/// - log progress towards, remaining blocks before, and remaining time to next network upgrade
/// - add some progress info to the metrics
pub async fn show_block_chain_progress(
    network: Network,
    latest_chain_tip: impl ChainTip,
    sync_status: SyncStatus,
) {
    // The minimum number of extra blocks after the highest checkpoint, based on:
    // - the non-finalized state limit, and
    // - the minimum number of extra blocks mined between a checkpoint update,
    //   and the automated tests for that update.
    let min_after_checkpoint_blocks = i32::try_from(zebra_state::MAX_BLOCK_REORG_HEIGHT)
        .expect("constant fits in i32")
        + MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE;

    // The minimum height of the valid best chain, based on:
    // - the hard-coded checkpoint height,
    // - the minimum number of blocks after the highest checkpoint.
    let after_checkpoint_height = CheckpointList::new(network)
        .max_height()
        .add(min_after_checkpoint_blocks)
        .expect("hard-coded checkpoint height is far below Height::MAX");

    let target_block_spacing = NetworkUpgrade::target_spacing_for_height(network, Height::MAX);
    let max_block_spacing =
        NetworkUpgrade::minimum_difficulty_spacing_for_height(network, Height::MAX);

    // We expect the state height to increase at least once in this interval.
    //
    // Most chain forks are 1-7 blocks long.
    //
    // TODO: remove the target_block_spacing multiplier,
    //       after fixing slow syncing near tip (#3375)
    let min_state_block_interval = max_block_spacing.unwrap_or(target_block_spacing * 4) * 2;

    // Formatted string for logging.
    let max_block_spacing = max_block_spacing
        .map(|duration| duration.to_string())
        .unwrap_or_else(|| "None".to_string());

    // The last time we downloaded and verified at least one block.
    //
    // Initialized to the start time to simplify the code.
    let mut last_state_change_time = Utc::now();

    // The state tip height, when we last downloaded and verified at least one block.
    //
    // Initialized to the genesis height to simplify the code.
    let mut last_state_change_height = Height(0);

    loop {
        let now = Utc::now();
        let is_syncer_stopped = sync_status.is_close_to_tip();

        if let Some(estimated_height) =
            latest_chain_tip.estimate_network_chain_tip_height(network, now)
        {
            // The estimate/actual race doesn't matter here,
            // because we're only using it for metrics and logging.
            let current_height = latest_chain_tip
                .best_tip_height()
                .expect("unexpected empty state: estimate requires a block height");
            let network_upgrade = NetworkUpgrade::current(network, current_height);

            // Work out the sync progress towards the estimated tip.
            let sync_progress = f64::from(current_height.0) / f64::from(estimated_height.0);
            let sync_percent = format!(
                "{:.frac$}%",
                sync_progress * 100.0,
                frac = SYNC_PERCENT_FRAC_DIGITS,
            );

            let remaining_sync_blocks = estimated_height - current_height;

            // Work out how long it has been since the state height has increased.
            //
            // Non-finalized forks can decrease the height, we only want to track increases.
            if current_height > last_state_change_height {
                last_state_change_height = current_height;
                last_state_change_time = now;
            }

            let time_since_last_state_block_chrono =
                now.signed_duration_since(last_state_change_time);
            let time_since_last_state_block = humantime_seconds(
                time_since_last_state_block_chrono
                    .to_std()
                    .unwrap_or_default(),
            );

            if time_since_last_state_block_chrono > min_state_block_interval {
                // The state tip height hasn't increased for a long time.
                //
                // Block verification can fail if the local node's clock is wrong.
                warn!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    %time_since_last_state_block,
                    %target_block_spacing,
                    %max_block_spacing,
                    ?is_syncer_stopped,
                    "chain updates have stalled, \
                     state height has not increased for {} minutes. \
                     Hint: check your network connection, \
                     and your computer clock and time zone",
                    time_since_last_state_block_chrono.num_minutes(),
                );
            } else if is_syncer_stopped && remaining_sync_blocks > MIN_SYNC_WARNING_BLOCKS {
                // We've stopped syncing blocks, but we estimate we're a long way from the tip.
                //
                // TODO: warn after fixing slow syncing near tip (#3375)
                info!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    ?remaining_sync_blocks,
                    ?after_checkpoint_height,
                    %time_since_last_state_block,
                    "initial sync is very slow, or estimated tip is wrong. \
                     Hint: check your network connection, \
                     and your computer clock and time zone",
                );
            } else if is_syncer_stopped && current_height <= after_checkpoint_height {
                // We've stopped syncing blocks,
                // but we're below the minimum height estimated from our checkpoints.
                let min_minutes_after_checkpoint_update: i64 = div_ceil(
                    i64::from(MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE)
                        * POST_BLOSSOM_POW_TARGET_SPACING,
                    60,
                );

                warn!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    ?remaining_sync_blocks,
                    ?after_checkpoint_height,
                    %time_since_last_state_block,
                    "initial sync is very slow, and state is below the highest checkpoint. \
                     Hint: check your network connection, \
                     and your computer clock and time zone. \
                     Dev Hint: were the checkpoints updated in the last {} minutes?",
                    min_minutes_after_checkpoint_update,
                );
            } else if is_syncer_stopped {
                // We've stayed near the tip for a while, and we've stopped syncing lots of blocks.
                // So we're mostly using gossiped blocks now.
                info!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    ?remaining_sync_blocks,
                    %time_since_last_state_block,
                    "finished initial sync to chain tip, using gossiped blocks",
                );
            } else if remaining_sync_blocks <= MAX_CLOSE_TO_TIP_BLOCKS {
                // We estimate we're near the tip, but we have been syncing lots of blocks recently.
                // We might also be using some gossiped blocks.
                info!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    ?remaining_sync_blocks,
                    %time_since_last_state_block,
                    "close to finishing initial sync, \
                     confirming using syncer and gossiped blocks",
                );
            } else {
                // We estimate we're far from the tip, and we've been syncing lots of blocks.
                info!(
                    %sync_percent,
                    ?current_height,
                    ?network_upgrade,
                    ?remaining_sync_blocks,
                    %time_since_last_state_block,
                    "estimated progress to chain tip",
                );
            }
        } else {
            let sync_percent = format!("{:.frac$} %", 0.0f64, frac = SYNC_PERCENT_FRAC_DIGITS,);

            if is_syncer_stopped {
                // We've stopped syncing blocks,
                // but we haven't downloaded and verified the genesis block.
                warn!(
                    %sync_percent,
                    current_height = %"None",
                    "initial sync can't download and verify the genesis block. \
                     Hint: check your network connection, \
                     and your computer clock and time zone",
                );
            } else {
                // We're waiting for the genesis block to be committed to the state,
                // before we can estimate the best chain tip.
                info!(
                    %sync_percent,
                    current_height = %"None",
                    "initial sync is waiting to download the genesis block",
                );
            }
        }

        tokio::time::sleep(LOG_INTERVAL).await;
    }
}
