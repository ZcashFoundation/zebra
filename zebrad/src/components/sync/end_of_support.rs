//! End of support checking task.

use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::Report;

use zebra_chain::{
    block::Height,
    chain_tip::{ChainTip, NetworkChainTipHeightEstimator},
    parameters::Network,
};

use crate::application::release_version;

/// The estimated height that this release will be published.
pub const ESTIMATED_RELEASE_HEIGHT: u32 = 3_444_000;

/// The maximum number of days after `ESTIMATED_RELEASE_HEIGHT` where a Zebra server will run
/// without halting.
///
/// Notes:
///
/// - Zebra will exit with a panic if the current tip height is bigger than the
///   `ESTIMATED_RELEASE_HEIGHT` plus this number of days.
/// - Currently set to 12 weeks.
pub const EOS_PANIC_AFTER: u32 = 84;

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

/// Returns the estimated last supported height for this release, or `None` on networks where
/// end of support is not enforced.
///
/// The node runs up to and including this height, and halts with an end of support panic when
/// the tip goes past it. This matches zcashd, where `end_of_service.block_height` is also the
/// threshold rather than the first halted block.
pub fn end_of_support_height(network: &Network) -> Option<Height> {
    (network == &Network::Mainnet).then(|| estimated_height_after_release(network, EOS_PANIC_AFTER))
}

/// Returns the estimated height `days` after [`ESTIMATED_RELEASE_HEIGHT`] on `network`,
/// following the target block spacing in force at each height.
pub fn estimated_height_after_release(network: &Network, days: u32) -> Height {
    // Only the elapsed time matters, so any reference time works.
    let release_time = DateTime::<Utc>::UNIX_EPOCH;
    NetworkChainTipHeightEstimator::new(release_time, Height(ESTIMATED_RELEASE_HEIGHT), network)
        .estimate_height_at(release_time + chrono::Duration::days(days.into()))
}

/// Check if the current release is too old and panic if so.
pub fn check(tip_height: Height, network: &Network) {
    info!("Checking if Zebra release is inside support range ...");

    let panic_height = estimated_height_after_release(network, EOS_PANIC_AFTER);
    let warn_height = estimated_height_after_release(network, EOS_WARN_AFTER);

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
