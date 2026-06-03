//! Standalone finalized-state rollback utility.

// This binary reports startup and rollback failures directly to the terminal before any tracing
// subscriber is installed.
#![allow(clippy::print_stderr)]

use clap::Parser;
use zebrad::commands::rollback_state::RollbackStateCmd;

/// Process entry point for `zebra-rollback-state`.
fn main() {
    if let Err(error) = color_eyre::install() {
        eprintln!("failed to install error handler: {error}");
        std::process::exit(1);
    }

    let command = RollbackStateCmd::parse();

    if let Err(error) = command.run_with_config(
        zebra_state::Config::default(),
        zebra_consensus::Config::default(),
    ) {
        eprintln!("failed to roll back finalized state: {error}");
        std::process::exit(1);
    }
}
