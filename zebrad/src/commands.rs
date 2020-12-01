//! Zebrad Subcommands

mod generate;
mod start;
mod version;

use self::ZebradCmd::*;
use self::{generate::GenerateCmd, start::StartCmd, version::VersionCmd};

use crate::config::ZebradConfig;

use abscissa_core::{
    config::Override, Command, Configurable, FrameworkError, Help, Options, Runnable,
};
use std::path::PathBuf;

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

/// Zebrad Subcommands
#[derive(Command, Debug, Options)]
pub enum ZebradCmd {
    /// The `generate` subcommand
    #[options(help = "generate a skeleton configuration")]
    Generate(GenerateCmd),

    /// The `help` subcommand
    #[options(help = "get usage information")]
    Help(Help<Self>),

    /// The `start` subcommand
    #[options(help = "start the application")]
    Start(StartCmd),

    /// The `version` subcommand
    #[options(help = "display version information")]
    Version(VersionCmd),
}

impl ZebradCmd {
    pub const GIT_COMMIT: &'static str = env!("VERGEN_SHA_SHORT");

    /// Returns true if this command is a server command.
    ///
    /// For example, `Start` acts as a Zcash node.
    pub(crate) fn is_server(&self) -> bool {
        match self {
            // List all the commands, so new commands have to make a choice here
            Start(_) => true,
            Generate(_) | Help(_) | Version(_) => false,
        }
    }
}

impl Runnable for ZebradCmd {
    fn run(&self) {
        let span = error_span!("git", SHA = %Self::SHA);
        let _guard = span.enter();
        match self {
            Generate(cmd) => cmd.run(),
            ZebradCmd::Help(cmd) => cmd.run(),
            Start(cmd) => cmd.run(),
            Version(cmd) => cmd.run(),
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
