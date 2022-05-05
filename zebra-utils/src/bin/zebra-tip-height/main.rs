//! Prints the chain tip height stored in Zebra's state.
//!
//! For usage please refer to the program help: `zebra-tip-height --help`

use color_eyre::eyre::{eyre, Result};
use structopt::StructOpt;

use zebra_chain::chain_tip::ChainTip;
use zebra_state::LatestChainTip;
use zebra_utils::init_tracing;

mod args;
use self::args::Args;

/// `zebra-tip-height` entrypoint.
///
/// Reads the chain tip height from Zebra's state in a cache directory (see [`Args`] for more
/// information).
#[allow(clippy::print_stdout)]
fn main() -> Result<()> {
    init_tracing();

    color_eyre::install()?;

    let args = Args::from_args();
    let latest_chain_tip = load_latest_chain_tip(args);

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    println!("{}", chain_tip_height.0);

    Ok(())
}

/// Starts a state service using the `cache_dir` and `network` from the provided [`Args`].
pub fn load_latest_chain_tip(args: Args) -> LatestChainTip {
    let mut config = zebra_state::Config::default();

    if let Some(cache_dir) = args.cache_dir {
        config.cache_dir = cache_dir;
    }

    let (_state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::init(config, args.network);

    latest_chain_tip
}
