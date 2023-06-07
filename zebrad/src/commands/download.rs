//! `download` subcommand - pre-download required parameter files
//!
//! `zebrad download` automatically downloads required parameter files the first time it is run.
//!
//! This command should be used if you're launching lots of `zebrad start` instances for testing,
//! or you want to include the parameter files in a distribution package.

use abscissa_core::{Command, Runnable};

/// Pre-download required Zcash Sprout and Sapling parameter files
#[derive(Command, Debug, Default, clap::Parser)]
pub struct DownloadCmd {}

impl DownloadCmd {
    /// Download the Sapling and Sprout Groth16 parameters if needed,
    /// check they were downloaded correctly, and load them into Zebra.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn download_and_check(&self) {
        // The lazy static initializer does the download, if needed,
        // and the file hash checks.
        lazy_static::initialize(&zebra_consensus::groth16::GROTH16_PARAMETERS);
    }
}

impl Runnable for DownloadCmd {
    /// Run the download command.
    fn run(&self) {
        info!("checking if Zcash Sapling and Sprout parameters have been downloaded");

        self.download_and_check();
    }
}
