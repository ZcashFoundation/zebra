//! Zebrad Subcommands

mod connect;
mod generate;
mod revhex;
mod seed;
mod start;
mod version;

use self::ZebradCmd::*;
use self::{
    connect::ConnectCmd, generate::GenerateCmd, revhex::RevhexCmd, seed::SeedCmd, start::StartCmd,
    version::VersionCmd,
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
    /// The `generate` subcommand
    #[options(help = "generate a skeleton configuration")]
    Generate(GenerateCmd),

    /// The `connect` subcommand
    #[options(help = "testing stub for dumping network messages")]
    Connect(ConnectCmd),

    /// The `help` subcommand
    #[options(help = "get usage information")]
    Help(Help<Self>),

    /// The `revhex` subcommand
    #[options(help = "reverses the endianness of a hex string, like a block or transaction hash")]
    Revhex(RevhexCmd),

    /// The `seed` subcommand
    #[options(help = "dns seeder")]
    Seed(SeedCmd),

    /// The `start` subcommand
    #[options(help = "start the application")]
    Start(StartCmd),

    /// The `version` subcommand
    #[options(help = "display version information")]
    Version(VersionCmd),
}

impl ZebradCmd {
    /// Returns true if this command sends output to stdout.
    ///
    /// For example, `Generate` sends a default config file to stdout.
    ///
    /// Usage note: `abscissa_core::EntryPoint` stores an `Option<ZerbradCmd>`.
    /// If the command is `None`, then abscissa writes zebrad usage information
    /// to stdout.
    pub(crate) fn uses_stdout(&self) -> bool {
        match self {
            // List all the commands, so new commands have to make a choice here
            Generate(_) | Help(_) | Revhex(_) | Version(_) => true,
            Connect(_) | Seed(_) | Start(_) => false,
        }
    }

    /// Returns true if this command is a server command.
    ///
    /// For example, `Start` acts as a Zcash node.
    ///
    /// Usage note: `abscissa_core::EntryPoint` stores an `Option<ZerbradCmd>`.
    /// If the command is `None`, then abscissa prints zebrad's usage
    /// information, then exits.
    pub(crate) fn is_server(&self) -> bool {
        match self {
            // List all the commands, so new commands have to make a choice here
            Connect(_) | Seed(_) | Start(_) => true,
            Generate(_) | Help(_) | Revhex(_) | Version(_) => false,
        }
    }
}

/// This trait allows you to define how application configuration is loaded.
impl Configurable<ZebradConfig> for ZebradCmd {
    /// Location of the configuration file
    fn config_path(&self) -> Option<PathBuf> {
        let if_exists = |f: PathBuf| if f.exists() { Some(f) } else { None };

        dirs::preference_dir()
            .map(|path| path.join(CONFIG_FILE))
            .and_then(if_exists)
            .or_else(|| std::env::current_dir().ok())
            .map(|path| path.join(CONFIG_FILE))
            .and_then(if_exists)

        // Note: Changes in how configuration is loaded may need usage
        // edits in generate.rs
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
