//! `start` subcommand - example of how to write a subcommand

/// App-local prelude includes `app_reader()`/`app_writer()`/`app_config()`
/// accessors along with logging macros. Customize as you see fit.
use crate::prelude::*;

use crate::config::ZebradConfig;
use abscissa_core::{config, Command, FrameworkError, Options, Runnable};

/// `start` subcommand
///
/// The `Options` proc macro generates an option parser based on the struct
/// definition, and is defined in the `gumdrop` crate. See their documentation
/// for a more comprehensive example:
///
/// <https://docs.rs/gumdrop/>
#[derive(Command, Debug, Options)]
pub struct StartCmd {
    /// Filter strings
    #[options(free)]
    filters: Vec<String>,
}

impl Runnable for StartCmd {
    /// Start the application.
    fn run(&self) {
        let config = app_config();
        println!("filter: {}!", &config.tracing.filter);

        let default_config = ZebradConfig::default();
        println!("Default config: {:?}", default_config);

        println!("Toml:\n{}", toml::to_string(&default_config).unwrap());
    }
}

impl config::Override<ZebradConfig> for StartCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(
        &self,
        mut config: ZebradConfig,
    ) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = self.filters.join(",");
        }

        Ok(config)
    }
}
