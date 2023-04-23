//! End of support checking task.

use std::time::Duration;

use color_eyre::Report;

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
};

/// The name of the current Zebra release.
pub const RELEASE_NAME: &str = "Zebra:1.0.0-rc.6";

/// The estimated height that this release started to run.
pub const ESTIMATED_RELEASE_HEIGHT: u32 = 2_026_000;

/// The maximum number of days after `ESTIMATED_RELEASE_HEIGHT` where a Zebra server runs.
///
/// Notes:
///
/// - Zebra will exit with a panic if the current tip height is bigger than the `ESTIMATED_RELEASE_HEIGHT`
///  plus this number of days.
pub const EOS_PANIC_AFTER: u32 = 180;

/// The number of days before the end of support where Zebra will display warnings.
pub const EOS_WARN_AFTER: u32 = EOS_PANIC_AFTER - 14;

/// A string which is part of the panic that will be displayed if Zebra release is too old.
pub const EOS_PANIC_MESSAGE_HEADER: &str = "Zebra refuses to run";

/// The amount of time between end of support checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Wait a few seconds at startup so `best_tip_height` is always `Some`.
const INITIAL_WAIT: Duration = Duration::from_secs(10);

/// Start the end of support checking task.
pub async fn start(
    network: Network,
    latest_chain_tip: impl ChainTip + std::fmt::Debug,
) -> Result<(), Report> {
    info!("Starting end of support task");

    tokio::time::sleep(INITIAL_WAIT).await;

    loop {
        if let Some(tip_height) = latest_chain_tip.best_tip_height() {
            check(tip_height, network);
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Check if the current release is too old and panic if so.
pub fn check(tip_height: Height, network: Network) {
    debug!("Checking if Zebra release is inside support range ...");

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
            "Your Zebra release is too old and it will stop running at block {}. \
            \nRelease name: {RELEASE_NAME}, Estimated release height: {ESTIMATED_RELEASE_HEIGHT} \
            \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest", panic_height.0
        );
    } else {
        debug!("Zebra release is under support");
    }
}
