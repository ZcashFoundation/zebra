//! `start` subcommand - example of how to write a subcommand

/// App-local prelude includes `app_reader()`/`app_writer()`/`app_config()`
/// accessors along with logging macros. Customize as you see fit.
use crate::prelude::*;

use crate::config::ZebradConfig;

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};

use futures::prelude::*;

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
        warn!("starting application");
        let config = app_config();
        if let Some(filter) = &config.tracing.filter {
            println!("filter: {}!", filter);
        }

        let default_config = ZebradConfig::default();
        println!("Default config: {:?}", default_config);

        println!("Toml:\n{}", toml::Value::try_from(&default_config).unwrap());

        info!("Starting placeholder loop");

        use crate::components::tokio::TokioComponent;

        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .block_on(future::pending::<()>());
    }
}

impl config::Override<ZebradConfig> for StartCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(&self, mut config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = Some(self.filters.join(","));
        }

        Ok(config)
    }
}
