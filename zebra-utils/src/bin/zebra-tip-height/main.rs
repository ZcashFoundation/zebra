//! Prints the chain tip height stored in Zebra's state.
//!
//! For usage please refer to the program help: `zebra-tip-height --help`

use color_eyre::eyre::{eyre, Result};
use structopt::StructOpt;
use tower::util::BoxService;

use zebra_chain::chain_tip::ChainTip;
use zebra_state::LatestChainTip;
use zebra_utils::init_tracing;

mod args;
use self::args::Args;

/// Type alias for a boxed state service.
type BoxStateService =
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>;

/// `zebra-tip-height` entrypoint.
///
/// Reads the chain tip height from Zebra's state in a cache directory (see [`Args`] for more
/// information).
#[allow(clippy::print_stdout)]
fn main() -> Result<()> {
    init_tracing();

    color_eyre::install()?;

    let args = Args::from_args();
    let (_state_service, latest_chain_tip) = start_state_service(args);

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    println!("{}", chain_tip_height.0);

    Ok(())
}

/// Starts a state service using the `cache_dir` and `network` from the provided [`Args`].
pub fn start_state_service(args: Args) -> (BoxStateService, LatestChainTip) {
    let config = zebra_state::Config {
        cache_dir: args.cache_dir,
        ..zebra_state::Config::default()
    };

    let (state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::init(config, args.network);

    (state_service, latest_chain_tip)
}
