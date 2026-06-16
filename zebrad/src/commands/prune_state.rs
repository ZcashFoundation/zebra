//! `prune-state` subcommand - prune finalized raw transaction data on disk.

use std::path::PathBuf;

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::Result;

use zebra_chain::parameters::Network;
use zebra_state::{PruneFinalizedStateOptions, PruneFinalizedStateSummary};

use crate::prelude::APPLICATION;

/// Prune historical raw transaction data from Zebra's finalized state
#[derive(Command, Debug, Default, Parser)]
pub struct PruneStateCmd {
    /// Number of recent finalized blocks below the tip whose raw transaction data is retained.
    #[clap(long, help = "recent finalized blocks below the tip to keep")]
    tx_retention: u32,

    /// Path to Zebra's cached state.
    ///
    /// The `zebrad prune-state` subcommand defaults to `state.cache_dir` from the loaded
    /// `zebrad.toml`, while the standalone `zebra-prune-state` binary defaults to
    /// `zebra_state::Config::default()`. Pass `--cache-dir` to be explicit and avoid pruning the
    /// wrong (or an empty) state directory.
    #[clap(long, short, help = "path to directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The network of the chain to prune.
    #[clap(
        long,
        short,
        required = true,
        help = "the network of the chain to load"
    )]
    network: Network,

    /// Actually prune the database. Without this flag, the command only previews the plan.
    #[clap(long, help = "apply the pruning plan instead of only previewing it")]
    confirm: bool,
}

impl Runnable for PruneStateCmd {
    /// `prune-state` sub-command entrypoint.
    fn run(&self) {
        let config = APPLICATION.config();

        if let Err(error) = self.run_with_config(config.state.clone()) {
            tracing::error!("Failed to prune finalized state: {error}");
            // Exit non-zero so scripts can detect failures.
            std::process::exit(1);
        }
    }
}

impl PruneStateCmd {
    /// Runs pruning using `state_config` as the base state configuration.
    #[allow(clippy::print_stdout)]
    pub fn run_with_config(&self, mut state_config: zebra_state::Config) -> Result<()> {
        if let Some(cache_dir) = self.cache_dir.clone() {
            state_config.cache_dir = cache_dir;
        }

        let options = PruneFinalizedStateOptions {
            tx_retention: self.tx_retention,
        };

        if self.confirm {
            let summary = zebra_state::prune_finalized_state(state_config, &self.network, options)?;
            print_summary("pruning complete", &summary);
        } else {
            let preview =
                zebra_state::preview_prune_finalized_state(state_config, &self.network, options)?;
            print_summary("pruning plan", &preview);
            println!("  no changes written: pass --confirm to apply this plan");
        }

        Ok(())
    }
}

#[allow(clippy::print_stdout)]
fn print_summary(label: &str, summary: &PruneFinalizedStateSummary) {
    println!("{label}:");
    println!(
        "  finalized tip: height {}, hash {}",
        summary.tip.0 .0, summary.tip.1
    );
    println!("  tx retention: {} blocks", summary.tx_retention);
    println!(
        "  previous lowest retained height: {}",
        optional_height(summary.previous_lowest_retained_height)
    );
    println!(
        "  new lowest retained height: {}",
        optional_height(summary.new_lowest_retained_height)
    );

    if summary.pruned_height_ranges.is_empty() {
        println!("  pruned height ranges: none");
    } else {
        println!(
            "  pruned height ranges: {} ({} heights)",
            summary.pruned_height_ranges.len(),
            summary.pruned_height_count
        );

        for (from, until) in &summary.pruned_height_ranges {
            println!("    [{}..{})", from.0, until.0);
        }
    }

    if let Some((from, until)) = summary.compacted_height_range {
        println!("  compacted raw tx range: [{}..{})", from.0, until.0);
    } else {
        println!("  compacted raw tx range: none");
    }
}

fn optional_height(height: Option<zebra_chain::block::Height>) -> String {
    height.map_or_else(|| "none".to_string(), |height| height.0.to_string())
}
