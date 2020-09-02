//! `generate` subcommand - generates a skeleton config.

use crate::config::ZebradConfig;
use abscissa_core::{Command, Options, Runnable};

/// `generate` subcommand
#[derive(Command, Debug, Options)]
pub struct GenerateCmd {
    /// The file to write the generated config to.
    #[options(help = "The file to write the generated config to (stdout if unspecified)")]
    output_file: Option<String>,
}

impl Runnable for GenerateCmd {
    /// Start the application.
    fn run(&self) {
        let default_config = ZebradConfig::default();
        let mut output = r"# Default configuration for zebrad.
#
# This file can be used as a skeleton for custom configs.
#
# This file is generated using zebrad's current defaults. If you want zebrad
# to automatically use any newer defaults, set the config options you want to
# keep, and delete the rest.
#
# The config format is documented here:
# https://doc.zebra.zfnd.org/zebrad/config/struct.ZebradConfig.html

# Usage:
#     zebrad generate -o myzebrad.toml
#     zebrad -c myzebrad.toml start
#
#     zebrad generate -o zebrad.toml
#     zebrad start
#
# If there is no -c flag on the command line, zebrad looks for zebrad.toml in
# the user's preference directory. If that file does not exist, zebrad uses the default
# config.

"
        .to_owned();

        // this avoids a ValueAfterTable error
        // https://github.com/alexcrichton/toml-rs/issues/145
        let conf = toml::Value::try_from(default_config).unwrap();
        output += &toml::to_string_pretty(&conf).expect("default config should be serializable");
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
