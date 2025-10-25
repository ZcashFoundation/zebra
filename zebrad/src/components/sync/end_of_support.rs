//! End of support checking task.

use std::time::Duration;

use color_eyre::Report;

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
};

use crate::application::release_version;

/// The estimated height that this release will be published.
pub const ESTIMATED_RELEASE_HEIGHT: u32 = 3_101_600;

/// The maximum number of days after `ESTIMATED_RELEASE_HEIGHT` where a Zebra server will run
/// without halting.
///
/// Notes:
///
/// - Zebra will exit with a panic if the current tip height is bigger than the
///   `ESTIMATED_RELEASE_HEIGHT` plus this number of days.
/// - Currently set to 5 weeks for release candidate.
/// - TODO: Revert to 15 weeks (105 days) for stable release.
pub const EOS_PANIC_AFTER: u32 = 35;

/// The number of days before the end of support where Zebra will display warnings.
pub const EOS_WARN_AFTER: u32 = EOS_PANIC_AFTER - 14;

/// A string which is part of the panic that will be displayed if Zebra halts.
pub const EOS_PANIC_MESSAGE_HEADER: &str = "Zebra refuses to run";

/// A string which is part of the warning that will be displayed if Zebra release is close to halting.
pub const EOS_WARN_MESSAGE_HEADER: &str = "Your Zebra release is too old and it will stop running";

/// The amount of time between end of support checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Wait a few seconds at startup so `best_tip_height` is always `Some`.
const INITIAL_WAIT: Duration = Duration::from_secs(10);

/// Start the end of support checking task for Mainnet.
pub async fn start(
    network: Network,
    latest_chain_tip: impl ChainTip + std::fmt::Debug,
) -> Result<(), Report> {
    info!("Starting end of support task");

    tokio::time::sleep(INITIAL_WAIT).await;

    loop {
        if network == Network::Mainnet {
            if let Some(tip_height) = latest_chain_tip.best_tip_height() {
                check(tip_height, &network);
            }
        } else {
            info!("Release always valid in Testnet");
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Check if the current release is too old and panic if so.
pub fn check(tip_height: Height, network: &Network) {
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
            \nRelease name: {}, Estimated release height: {ESTIMATED_RELEASE_HEIGHT} \
            \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest",
            release_version()
        );
    } else if tip_height > warn_height {
        warn!(
            "{EOS_WARN_MESSAGE_HEADER} at block {}. \
            \nRelease name: {}, Estimated release height: {ESTIMATED_RELEASE_HEIGHT} \
            \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest", panic_height.0, release_version()
        );
    } else {
        info!("Zebra release is supported until block {}, please report bugs at https://github.com/ZcashFoundation/zebra/issues", panic_height.0);
    }
}
