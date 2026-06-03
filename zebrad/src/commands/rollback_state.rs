//! `rollback-state` subcommand - roll back Zebra's finalized chain state on disk.

use std::path::PathBuf;

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::Result;

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
    ///
    /// The `zebrad rollback-state` subcommand defaults to `state.cache_dir` from the loaded
    /// `zebrad.toml`, while the standalone `zebra-rollback-state` binary defaults to
    /// `zebra_state::Config::default()`. Pass `--cache-dir` to be explicit and avoid rolling back
    /// the wrong (or an empty) state directory.
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

    /// Preview the rollback plan without mutating finalized state.
    #[clap(long, help = "show the rollback plan without changing finalized state")]
    dry_run: bool,
}

impl Runnable for RollbackStateCmd {
    /// `rollback-state` sub-command entrypoint.
    fn run(&self) {
        let config = APPLICATION.config();

        if let Err(error) = self.run_with_config(config.state.clone(), config.consensus.clone()) {
            tracing::error!("Failed to roll back finalized state: {error}");
            // Exit non-zero so the subcommand matches the standalone `zebra-rollback-state` binary
            // and scripts can detect the failure (abscissa would otherwise exit 0).
            std::process::exit(1);
        }
    }
}

impl RollbackStateCmd {
    /// Runs rollback using `state_config` as the base state configuration.
    ///
    /// `consensus_config` supplies `checkpoint_sync`, which sets the max checkpoint height used to
    /// guard `--keep-rolled-back-blocks`: blocks kept below it would be discarded on the next start.
    #[allow(clippy::print_stdout)]
    pub fn run_with_config(
        &self,
        mut state_config: zebra_state::Config,
        consensus_config: zebra_consensus::config::Config,
    ) -> Result<()> {
        if let Some(cache_dir) = self.cache_dir.clone() {
            state_config.cache_dir = cache_dir;
        }

        let (_, max_checkpoint_height) =
            zebra_consensus::router::init_checkpoint_list(consensus_config, &self.network);

        let options = RollbackFinalizedStateOptions {
            target_height: block::Height(self.height),
            keep_rolled_back_blocks: self.keep_rolled_back_blocks,
            max_checkpoint_height: Some(max_checkpoint_height),
        };

        if self.dry_run {
            let preview = zebra_state::preview_rollback_finalized_state(
                state_config,
                &self.network,
                options,
            )?;
            print_summary("rollback plan", &preview);
            return Ok(());
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
