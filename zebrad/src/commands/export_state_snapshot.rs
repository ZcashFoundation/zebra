//! `export-state-snapshot` subcommand - exports a state snapshot of a stopped
//! node's finalized state.
//!
//! The state is opened read-only, so the node must be stopped, and its
//! finalized tip must be exactly the snapshot height. The snapshot's pieces
//! and manifest are written to the artifact store under the state cache
//! directory, and the commitment that names the snapshot is printed, for other
//! nodes to configure or a release to pin.
//!
//! A state in the previous major format that this build reads without a
//! migration is exported in place, in the current encodings; the tests export
//! current-format states, and the Mainnet benchmark covers that path.

use std::{
    fs,
    path::{Path, PathBuf},
};

use abscissa_core::{Application, Command, Runnable, Shutdown};
use clap::Parser;
use color_eyre::eyre::{Result, WrapErr};

use zebra_chain::{block, state_snapshot::StateSnapshotCommitment};
use zebra_state::state_snapshot::{export::export_state_snapshot, store::ArtifactStore};

use crate::prelude::APPLICATION;

#[cfg(test)]
mod tests;

/// Export a state snapshot at the finalized tip, for other nodes to bootstrap from (expert users
/// only)
#[derive(Command, Debug, Default, Parser)]
pub struct ExportStateSnapshotCmd {
    /// The snapshot height, which must be the finalized tip of the state.
    #[clap(
        long,
        help = "the height to export the state at, which must be its finalized tip"
    )]
    height: u32,

    /// Path to Zebra's cached state.
    #[clap(long, short, help = "path to directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The cache directory to write the snapshot under, as
    /// `<DIR>/state-snapshots/<network>/`, instead of the state's.
    #[clap(
        long,
        value_name = "DIR",
        help = "the cache directory to write the snapshot under, as \
                <DIR>/state-snapshots/<network>/ (default: the state cache directory, \
                which `serve_state_snapshots` serves from)"
    )]
    out: Option<PathBuf>,

    /// Write only the commitment's TOML to this file, for machine consumption.
    #[clap(long, value_name = "PATH")]
    commitment_out: Option<PathBuf>,
}

impl Runnable for ExportStateSnapshotCmd {
    /// `export-state-snapshot` sub-command entrypoint.
    #[allow(clippy::print_stdout, clippy::print_stderr)]
    fn run(&self) {
        let result = self
            .export()
            .and_then(|commitment| self.output_commitment(&commitment));
        if let Err(error) = result {
            // Snapshot errors include their sources in their messages.
            eprintln!("state snapshot export failed: {error}");
            APPLICATION.shutdown(Shutdown::Forced);
        }
    }
}

impl ExportStateSnapshotCmd {
    /// Exports the snapshot into the artifact store under the output cache directory, and
    /// returns its commitment.
    fn export(&self) -> Result<StateSnapshotCommitment> {
        let app_config = APPLICATION.config();
        let mut config = app_config.state.clone();
        if let Some(cache_dir) = self.cache_dir.clone() {
            config.cache_dir = cache_dir;
        }
        let network = &app_config.network.network;

        let store_config = zebra_state::Config {
            cache_dir: self.out.clone().unwrap_or_else(|| config.cache_dir.clone()),
            ..config.clone()
        };
        let store = ArtifactStore::new(&store_config, network);

        Ok(export_state_snapshot(
            &config,
            network,
            block::Height(self.height),
            &store,
        )?)
    }

    /// Prints the commitment, or writes a log-free TOML file when requested.
    #[allow(clippy::print_stdout)]
    fn output_commitment(&self, commitment: &StateSnapshotCommitment) -> Result<()> {
        let toml = commitment_toml(commitment);
        if let Some(path) = &self.commitment_out {
            write_commitment(path, &toml)
        } else {
            print!("{toml}");
            Ok(())
        }
    }
}

/// Writes a complete commitment TOML document to `path`.
fn write_commitment(path: &Path, toml: &str) -> Result<()> {
    fs::write(path, toml)
        .wrap_err_with(|| format!("could not write the commitment to {}", path.display()))
}

/// Returns `commitment` as the `[sync.state_snapshot]` table of a `zebrad.toml`
/// config, with a comment saying where it goes.
///
/// The table's lines are `height = ..`, `hash = ".."` and `manifest = ".."`.
fn commitment_toml(commitment: &StateSnapshotCommitment) -> String {
    let table =
        toml::to_string(commitment).expect("a commitment is a table of a number and strings");
    format!(
        "# Add this table to zebrad.toml, after the keys of its [sync] section:\n\
         [sync.state_snapshot]\n\
         {table}"
    )
}
