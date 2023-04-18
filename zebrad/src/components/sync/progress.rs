//! Progress tracking for blockchain syncing.

use std::{cmp::min, ops::Add, time::Duration};

use chrono::{TimeZone, Utc};
use num_integer::div_ceil;

use zebra_chain::{
    block::{Height, HeightDiff},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    fmt::humantime_seconds,
    parameters::{Network, NetworkUpgrade, POST_BLOSSOM_POW_TARGET_SPACING},
};
use zebra_consensus::CheckpointList;
use zebra_network::constants::{
    EOS_PANIC_AFTER, EOS_PANIC_MESSAGE_HEADER, EOS_WARN_AFTER, ESTIMATED_RELEASE_HEIGHT,
    RELEASE_NAME,
};
use zebra_state::MAX_BLOCK_REORG_HEIGHT;

use crate::components::sync::SyncStatus;

/// The amount of time between progress logs.
const LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The amount of time between progress bar updates.
const PROGRESS_BAR_INTERVAL: Duration = Duration::from_secs(5);

/// The number of blocks we consider to be close to the tip.
///
/// Most chain forks are 1-7 blocks long.
const MAX_CLOSE_TO_TIP_BLOCKS: HeightDiff = 1;

/// Skip slow sync warnings when we are this close to the tip.
///
/// In testing, we've seen warnings around 30 blocks.
///
/// TODO: replace with `MAX_CLOSE_TO_TIP_BLOCKS` after fixing slow syncing near tip (#3375)
const MIN_SYNC_WARNING_BLOCKS: HeightDiff = 60;

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
//
// TODO: change to HeightDiff?
const MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE: u32 = 10;

/// Logs Zebra's estimated progress towards the chain tip every minute or so, and
/// updates a terminal progress bar every few seconds.
///
/// TODO:
/// - log progress towards, remaining blocks before, and remaining time to next network upgrade
/// - add some progress info to the metrics
pub async fn show_block_chain_progress(
    network: Network,
    latest_chain_tip: impl ChainTip,
    sync_status: SyncStatus,
) -> ! {
    // The minimum number of extra blocks after the highest checkpoint, based on:
    // - the non-finalized state limit, and
    // - the minimum number of extra blocks mined between a checkpoint update,
    //   and the automated tests for that update.
    let min_after_checkpoint_blocks =
        MAX_BLOCK_REORG_HEIGHT + MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE;
    let min_after_checkpoint_blocks: HeightDiff = min_after_checkpoint_blocks.into();

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

    // Formatted strings for logging.
    let target_block_spacing = humantime_seconds(
        target_block_spacing
            .to_std()
            .expect("constant fits in std::Duration"),
    );
    let max_block_spacing = max_block_spacing
        .map(|duration| {
            humantime_seconds(duration.to_std().expect("constant fits in std::Duration"))
        })
        .unwrap_or_else(|| "None".to_string());

    // The last time we downloaded and verified at least one block.
    //
    // Initialized to the start time to simplify the code.
    let mut last_state_change_time = Utc::now();

    // The state tip height, when we last downloaded and verified at least one block.
    //
    // Initialized to the genesis height to simplify the code.
    let mut last_state_change_height = Height(0);

    // The last time we logged an update.
    // Initialised with the unix epoch, to simplify the code while still staying in the std range.
    let mut last_log_time = Utc
        .timestamp_opt(0, 0)
        .single()
        .expect("in-range number of seconds and valid nanosecond");

    #[cfg(feature = "progress-bar")]
    let block_bar = howudoin::new().label("Blocks");

    loop {
        let now = Utc::now();
        let is_syncer_stopped = sync_status.is_close_to_tip();

        // Check for the end of support of this release.
        if let Some(tip_height) = latest_chain_tip.best_tip_height() {
            end_of_support(tip_height, network);
        }

        if let Some(estimated_height) =
            latest_chain_tip.estimate_network_chain_tip_height(network, now)
        {
            // The estimate/actual race doesn't matter here,
            // because we're only using it for metrics and logging.
            let current_height = latest_chain_tip
                .best_tip_height()
                .expect("unexpected empty state: estimate requires a block height");
            let network_upgrade = NetworkUpgrade::current(network, current_height);

            // Send progress reports for block height
            #[cfg(feature = "progress-bar")]
            if matches!(howudoin::cancelled(), Some(true)) {
                block_bar.close();
            } else {
                block_bar
                    .set_pos(current_height.0)
                    .set_len(u64::from(estimated_height.0))
                    .desc(network_upgrade.to_string());
            }

            // Skip logging if it isn't time for it yet
            let elapsed_since_log = (now - last_log_time)
                .to_std()
                .expect("elapsed times are in range");
            if elapsed_since_log < LOG_INTERVAL {
                continue;
            } else {
                last_log_time = now;
            }

            // Work out the sync progress towards the estimated tip.
            let sync_progress = f64::from(current_height.0) / f64::from(estimated_height.0);
            let sync_percent = format!(
                "{:.frac$}%",
                sync_progress * 100.0,
                frac = SYNC_PERCENT_FRAC_DIGITS,
            );

            let mut remaining_sync_blocks = estimated_height - current_height;
            if remaining_sync_blocks < 0 {
                remaining_sync_blocks = 0;
            }

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

                // TODO: use add_warn(), but only add each warning once
                #[cfg(feature = "progress-bar")]
                block_bar.desc("chain updates have stalled");
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

                #[cfg(feature = "progress-bar")]
                block_bar.desc("sync is very slow, or estimated tip is wrong");
            } else if is_syncer_stopped && current_height <= after_checkpoint_height {
                // We've stopped syncing blocks,
                // but we're below the minimum height estimated from our checkpoints.
                let min_minutes_after_checkpoint_update = div_ceil(
                    MIN_BLOCKS_MINED_AFTER_CHECKPOINT_UPDATE * POST_BLOSSOM_POW_TARGET_SPACING,
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

                #[cfg(feature = "progress-bar")]
                block_bar.desc("sync is very slow");
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

                #[cfg(feature = "progress-bar")]
                block_bar.desc(format!("{}: initial sync finished", network_upgrade));
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

                #[cfg(feature = "progress-bar")]
                block_bar.desc(format!("{}: initial sync almost finished", network_upgrade));
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
            let sync_percent = format!("{:.SYNC_PERCENT_FRAC_DIGITS$} %", 0.0f64,);

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

                #[cfg(feature = "progress-bar")]
                block_bar.desc("can't download genesis block");
            } else {
                // We're waiting for the genesis block to be committed to the state,
                // before we can estimate the best chain tip.
                info!(
                    %sync_percent,
                    current_height = %"None",
                    "initial sync is waiting to download the genesis block",
                );

                #[cfg(feature = "progress-bar")]
                block_bar.desc("waiting to download genesis block");
            }
        }

        tokio::time::sleep(min(LOG_INTERVAL, PROGRESS_BAR_INTERVAL)).await;
    }
}

/// Check if the current release is too old and panic if so.
pub fn end_of_support(tip_height: Height, network: Network) {
    info!("Checking if Zebra release is inside support range ...");

    // Get the current block spacing
    let target_block_spacing = NetworkUpgrade::target_spacing_for_height(network, tip_height);

    // Get the number of blocks per day
    let estimated_blocks_per_day =
        u32::try_from(chrono::Duration::days(1).num_seconds() / target_block_spacing.num_seconds())
            .expect("number is always small enough to fit");

    let panic_height =
        Height(ESTIMATED_RELEASE_HEIGHT + (EOS_PANIC_AFTER * estimated_blocks_per_day));
    let warn_height =
        Height(ESTIMATED_RELEASE_HEIGHT + (EOS_WARN_AFTER * estimated_blocks_per_day));

    if tip_height > panic_height {
        panic!(
            "{EOS_PANIC_MESSAGE_HEADER} if the release date is older than {EOS_PANIC_AFTER} days. \
            \nRelease name: {RELEASE_NAME}, Estimated release height: {ESTIMATED_RELEASE_HEIGHT} \
            \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest"
        );
    } else if tip_height > warn_height {
        warn!(
            "Your Zebra release is too old and it will stop running in block {}. \
            \nRelease name: {RELEASE_NAME}, Estimated release height: {ESTIMATED_RELEASE_HEIGHT} \
            \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest", panic_height.0
        );
    } else {
        info!("Zebra release is under support");
    }
}
