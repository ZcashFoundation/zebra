//! `rollback-state` subcommand - roll back Zebra's finalized chain state on disk.

use std::path::PathBuf;

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::{eyre, Result};

use zebra_chain::{block, parameters::Network};
use zebra_state::{RollbackFinalizedStateOptions, RollbackFinalizedStateSummary};

use crate::prelude::APPLICATION;

/// Roll back Zebra's finalized state to a previous height
#[derive(Command, Debug, Default, Parser)]
pub struct RollbackStateCmd {
    /// Roll back finalized state to this block height.
    #[clap(long, help = "finalized block height to keep as the new tip")]
    height: u32,

    /// Path to Zebra's cached state.
    #[clap(long, short, help = "path to directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The network of the chain to roll back.
    #[clap(
        long,
        short,
        required = true,
        help = "the network of the chain to load"
    )]
    network: Network,

    /// Write removed finalized blocks to the non-finalized backup cache.
    #[clap(long, help = "keep rolled-back blocks for non-finalized restore")]
    keep_rolled_back_blocks: bool,

    /// Actually mutate the state after printing the rollback plan.
    #[clap(long, help = "required to mutate the finalized state")]
    force: bool,
}

impl Runnable for RollbackStateCmd {
    /// `rollback-state` sub-command entrypoint.
    fn run(&self) {
        let config = APPLICATION.config().state.clone();

        if let Err(error) = self.run_with_config(config) {
            tracing::error!("Failed to roll back finalized state: {error}");
        }
    }
}

impl RollbackStateCmd {
    /// Runs rollback using `state_config` as the base state configuration.
    #[allow(clippy::print_stdout)]
    pub fn run_with_config(&self, mut state_config: zebra_state::Config) -> Result<()> {
        if let Some(cache_dir) = self.cache_dir.clone() {
            state_config.cache_dir = cache_dir;
        }

        let options = RollbackFinalizedStateOptions {
            target_height: block::Height(self.height),
            keep_rolled_back_blocks: self.keep_rolled_back_blocks,
        };

        let preview = zebra_state::preview_rollback_finalized_state(
            state_config.clone(),
            &self.network,
            options.clone(),
        )?;

        print_summary("rollback plan", &preview);

        if !self.force {
            return Err(eyre!(
                "rollback-state requires --force after reviewing the rollback plan"
            ));
        }

        let summary = zebra_state::rollback_finalized_state(state_config, &self.network, options)?;
        print_summary("rollback complete", &summary);

        Ok(())
    }
}

#[allow(clippy::print_stdout)]
fn print_summary(label: &str, summary: &RollbackFinalizedStateSummary) {
    println!("{label}:");
    println!(
        "  old finalized tip: height {}, hash {}",
        summary.old_tip.0 .0, summary.old_tip.1
    );
    println!(
        "  new finalized tip: height {}, hash {}",
        summary.new_tip.0 .0, summary.new_tip.1
    );
    println!("  rolled-back blocks: {}", summary.rolled_back_count);

    if let Some(backup) = &summary.backup {
        println!(
            "  non-finalized backup: {} blocks in {}",
            backup.block_count,
            backup.path.display()
        );
    }
}
