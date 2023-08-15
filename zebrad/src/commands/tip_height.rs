//! `tip-height` subcommand - prints the block height of Zebra's persisted chain state.
//!
//! Prints the chain tip height stored in Zebra's state. This is useful for developers to inspect
//! Zebra state directories.

use std::path::PathBuf;

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::{eyre, Result};

use zebra_chain::{
    block::{self, Height},
    chain_tip::ChainTip,
    parameters::Network,
};
use zebra_state::LatestChainTip;

use crate::prelude::APPLICATION;

/// Print the tip block height of Zebra's chain state on disk
#[derive(Command, Debug, Default, Parser)]
pub struct TipHeightCmd {
    /// Path to Zebra's cached state.
    #[clap(long, short, help = "path to directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The network to obtain the chain tip.
    #[clap(long, short, help = "the network of the chain to load")]
    network: Network,
}

impl Runnable for TipHeightCmd {
    /// `tip-height` sub-command entrypoint.
    ///
    /// Reads the chain tip height from a cache directory with Zebra's state.
    #[allow(clippy::print_stdout)]
    fn run(&self) {
        match self.load_tip_height() {
            Ok(height) => println!("{}", height.0),
            Err(error) => tracing::error!("Failed to read chain tip height from state: {error}"),
        }
    }
}

impl TipHeightCmd {
    /// Load the chain tip height from the state cache directory.
    fn load_tip_height(&self) -> Result<block::Height> {
        let latest_chain_tip = self.load_latest_chain_tip();

        latest_chain_tip
            .best_tip_height()
            .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))
    }

    /// Starts a state service using the `cache_dir` and `network` from the provided arguments.
    fn load_latest_chain_tip(&self) -> LatestChainTip {
        let mut config = APPLICATION.config().state.clone();

        if let Some(cache_dir) = self.cache_dir.clone() {
            config.cache_dir = cache_dir;
        }

        // UTXO verification isn't used here: we're not updating the state.
        let (_state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
            zebra_state::init(config, self.network, Height::MAX, 0);

        latest_chain_tip
    }
}
