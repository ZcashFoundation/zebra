//! `download` subcommand - pre-download required parameter files
//!
//! `zebrad start` automatically downloads required paramter files the first time it is run.
//!
//! This command should be used if you're launching lots of `zebrad start` instances for testing,
//! or you want to include the parameter files in a distribution package.

use abscissa_core::{Command, Options, Runnable};

/// `start` subcommand
#[derive(Command, Debug, Default, Options)]
pub struct DownloadCmd {}

impl DownloadCmd {
    /// Download the Sapling and Sprout Groth16 parameters if needed,
    /// check they were downloaded correctly, and load them into Zebra.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn download_and_verify(&self) {
        let _ = zebra_consensus::Groth16Params::new();
    }
}

impl Runnable for DownloadCmd {
    /// Run the download command.
    fn run(&self) {
        info!("checking if Zcash Sapling and Sprout parameters have been downloaded");

        self.download_and_verify();
    }
}
