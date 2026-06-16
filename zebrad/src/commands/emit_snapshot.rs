//! `emit-snapshot` subcommand - emits IBD state-snapshot artifacts from a synced state.
//!
//! Reads a synced, read-only finalized state and writes assumeUTXO-style
//! snapshot artifacts (design doc §16/§17), so a node can load a prepared state
//! at the snapshot height instead of re-deriving it. Currently emits the set of
//! unspent transparent output locations at the finalized tip; note-commitment
//! tree artifacts follow.
//!
//! The snapshot height is the cached state's finalized tip: point this at a
//! state synced to (or rolled back to) the height you want to snapshot.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
};

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::{eyre, Result};

use zebra_chain::parameters::Network;

use crate::prelude::APPLICATION;

/// The name of the emitted unspent-transparent-output-location artifact.
const UNSPENT_OUTPUTS_FILE: &str = "unspent-output-locations.bin";

/// Emit IBD state-snapshot artifacts from a synced read-only state (expert users only)
#[derive(Command, Debug, Default, Parser)]
pub struct EmitSnapshotCmd {
    /// Path to Zebra's cached state directory.
    #[clap(long, short, help = "path to the directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The network of the cached state.
    #[clap(long, short, help = "the network of the chain to load")]
    network: Network,

    /// Output directory for the emitted snapshot artifacts.
    #[clap(long, short, help = "directory to write the snapshot artifacts into")]
    out_dir: PathBuf,
}

impl Runnable for EmitSnapshotCmd {
    /// `emit-snapshot` sub-command entrypoint.
    fn run(&self) {
        if let Err(error) = self.emit() {
            tracing::error!("failed to emit state snapshot: {error:?}");
        }
    }
}

impl EmitSnapshotCmd {
    /// Opens the cached state read-only and emits the configured artifacts.
    #[allow(clippy::print_stdout)]
    fn emit(&self) -> Result<()> {
        let mut config = APPLICATION.config().state.clone();
        if let Some(cache_dir) = self.cache_dir.clone() {
            config.cache_dir = cache_dir;
        }

        // `init_read_only` returns the read service, the finalized `ZebraDb`,
        // and the non-finalized sender; the snapshot only needs the finalized DB.
        let db = zebra_state::init_read_only(config, &self.network).1;

        let (tip_height, tip_hash) = db
            .tip()
            .ok_or_else(|| eyre!("the cached state has no chain tip block"))?;

        std::fs::create_dir_all(&self.out_dir)?;

        let unspent_outputs = self.emit_unspent_output_locations(&db)?;

        tracing::info!(
            ?tip_height,
            ?tip_hash,
            unspent_outputs,
            "emitted IBD state snapshot",
        );
        println!(
            "snapshot at tip height {} ({}):\n  {unspent_outputs} unspent transparent output \
             locations -> {}",
            tip_height.0,
            tip_hash,
            self.out_dir.join(UNSPENT_OUTPUTS_FILE).display(),
        );

        Ok(())
    }

    /// Streams every unspent transparent output location (8 bytes each, in
    /// ascending location order) to the artifact file. Returns the count.
    fn emit_unspent_output_locations(&self, db: &zebra_state::ZebraDb) -> Result<u64> {
        let path = self.out_dir.join(UNSPENT_OUTPUTS_FILE);
        let mut writer = BufWriter::new(File::create(&path)?);

        let mut count: u64 = 0;
        let mut write_error = None;
        db.for_each_unspent_output_location_bytes(|bytes| {
            // Stop writing after the first error; surfaced after the stream ends.
            if write_error.is_some() {
                return;
            }
            match writer.write_all(bytes) {
                Ok(()) => count += 1,
                Err(error) => write_error = Some(error),
            }
        });

        if let Some(error) = write_error {
            return Err(error.into());
        }

        writer.flush()?;
        Ok(count)
    }
}
