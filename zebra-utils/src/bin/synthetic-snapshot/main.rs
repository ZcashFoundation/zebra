//! Tool-only synthetic snapshot preparation and independent state verification.

// This binary prints reports and progress directly to its callers.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod config;
mod interrupt;
mod node;
mod prepare;
mod report;
mod run;
#[cfg(test)]
mod serving_tests;
mod verify;

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(about = "Prepare synthetic snapshots and independently verify installed Zebra state")]
enum Command {
    /// Build A, export, import into temporary state, and compare selected rows.
    Prepare {
        /// One JSON config; successful preparation updates its resolved section atomically.
        #[arg(long)]
        config: PathBuf,
    },
    /// Run fresh production bootstrap trials, comparing each stopped B against prepared A.
    Run {
        /// The same JSON config used to prepare A.
        #[arg(long)]
        config: PathBuf,
        /// Production zebrad executable (a path or a program on PATH).
        #[arg(long, default_value = "zebrad")]
        zebrad: PathBuf,
        /// New directory for trial databases, logs, and comparison results.
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare prepared A with an existing stopped B, without launching bootstrap.
    Verify {
        /// The same JSON config used to prepare A.
        #[arg(long)]
        config: PathBuf,
        /// B's state cache directory, relative to the current working directory.
        #[arg(long)]
        state: PathBuf,
    },
}

fn main() -> ExitCode {
    let command = Command::parse();
    let canonical = |config: PathBuf| config.canonicalize().map_err(zebra_state::BoxError::from);
    let result = match command {
        Command::Prepare { config } => canonical(config).and_then(|c| prepare::prepare(&c)),
        Command::Run {
            config,
            zebrad,
            output,
        } => run::command(&config, &zebrad, &output, true),
        Command::Verify { config, state } => {
            canonical(config).and_then(|c| verify::command(&c, &state))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("synthetic-snapshot: {error}");
            ExitCode::FAILURE
        }
    }
}
