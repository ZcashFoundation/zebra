//! Zebrad Subcommands
//!
//! This is where you specify the subcommands of your application.
//!
//! The default application comes with two subcommands:
//!
//! - `start`: launches the application
//! - `version`: print application version
//!
//! See the `impl Configurable` below for how to specify the path to the
//! application's configuration file.
// TODO: update the list of commands above or find a way to keep it
// automatically up to date.

mod config;
mod connect;
mod seed;
mod start;
mod version;

use self::{
    config::ConfigCmd, connect::ConnectCmd, seed::SeedCmd, start::StartCmd, version::VersionCmd,
};
use crate::config::ZebradConfig;
use abscissa_core::{
    config::Override, Command, Configurable, FrameworkError, Help, Options, Runnable,
};
use std::path::PathBuf;

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

/// Zebrad Subcommands
#[derive(Command, Debug, Options, Runnable)]
pub enum ZebradCmd {
    /// The `help` subcommand
    #[options(help = "get usage information")]
    Help(Help<Self>),

    /// The `start` subcommand
    #[options(help = "start the application")]
    Start(StartCmd),

    /// The `config` subcommand
    #[options(help = "generate a skeleton configuration")]
    Config(ConfigCmd),

    /// The `version` subcommand
    #[options(help = "display version information")]
    Version(VersionCmd),

    /// The `connect` subcommand
    #[options(help = "testing stub for dumping network messages")]
    Connect(ConnectCmd),

    /// The `seed` subcommand
    #[options(help = "dns seeder")]
    Seed(SeedCmd),
}

/// This trait allows you to define how application configuration is loaded.
impl Configurable<ZebradConfig> for ZebradCmd {
    /// Location of the configuration file
    fn config_path(&self) -> Option<PathBuf> {
        let filename = std::env::current_dir().ok().map(|mut dir_path| {
            dir_path.push(CONFIG_FILE);
            dir_path
        });

        let if_exists = |f: PathBuf| if f.exists() { Some(f) } else { None };

        filename.and_then(|f| if_exists(f))
    }

    /// Apply changes to the config after it's been loaded, e.g. overriding
    /// values in a config file using command-line options.
    ///
    /// This can be safely deleted if you don't want to override config
    /// settings from command-line options.
    fn process_config(&self, config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        match self {
            ZebradCmd::Start(cmd) => cmd.override_config(config),
            _ => Ok(config),
        }
    }
}
