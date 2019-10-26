//! `config` subcommand - generates a skeleton config.

use crate::config::ZebradConfig;
use abscissa_core::{Command, Options, Runnable};

/// `config` subcommand
#[derive(Command, Debug, Options)]
pub struct ConfigCmd {
    /// The file to write the generated config to.
    #[options(help = "The file to write the generated config to (stdout if unspecified)")]
    output_file: Option<String>,
}

impl Runnable for ConfigCmd {
    /// Start the application.
    fn run(&self) {
        let default_config = ZebradConfig::default();
        let mut output = r"# Default configuration values for zebrad.
#
# This file is intended as a skeleton for custom configs.
#
# Because this contains default values, and the default
# values may change, you should delete all entries except
# for the ones you wish to change.
#
# Documentation on the meanings of each config option
# can be found in Rustdoc. XXX add link to rendered docs.

"
        .to_owned(); // The default name and location of the config file is defined in ../commands.rs
        output += &toml::to_string_pretty(&default_config)
            .expect("default config should be serializable");
        match self.output_file {
            Some(ref output_file) => {
                use std::{fs::File, io::Write};
                File::create(output_file)
                    .expect("must be able to open output file")
                    .write_all(output.as_bytes())
                    .expect("must be able to write output");
            }
            None => {
                println!("{}", output);
            }
        }
    }
}
