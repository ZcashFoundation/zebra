//! Zebrad Subcommands

mod copy_state;
mod download;
mod generate;
mod start;
mod tip_height;
mod version;

use self::ZebradCmd::*;
use self::{
    copy_state::CopyStateCmd, download::DownloadCmd, generate::GenerateCmd, start::StartCmd,
    tip_height::TipHeightCmd, version::VersionCmd,
};

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
    /// The `copy-state` subcommand, used to debug cached chain state
    // TODO: hide this command from users in release builds (#3279)
    #[options(help = "copy cached chain state (debug only)")]
    CopyState(CopyStateCmd),

    /// The `download` subcommand
    #[options(help = "pre-download required parameter files")]
    Download(DownloadCmd),

    /// The `generate` subcommand
    #[options(help = "generate a skeleton configuration")]
    Generate(GenerateCmd),

    /// The `help` subcommand
    #[options(help = "get usage information")]
    Help(Help<Self>),

    /// The `start` subcommand
    #[options(help = "start the application")]
    Start(StartCmd),

    /// The `tip-height` subcommand
    #[options(help = "get the block height of Zebra's persisted chain state")]
    TipHeight(TipHeightCmd),

    /// The `version` subcommand
    #[options(help = "display version information")]
    Version(VersionCmd),
}

impl ZebradCmd {
    /// Returns true if this command is a server command.
    ///
    /// For example, `Start` acts as a Zcash node.
    pub(crate) fn is_server(&self) -> bool {
        match self {
            // List all the commands, so new commands have to make a choice here
            CopyState(_) | Start(_) => true,
            Download(_) | Generate(_) | Help(_) | TipHeight(_) | Version(_) => false,
        }
    }
}

impl Runnable for ZebradCmd {
    fn run(&self) {
        match self {
            CopyState(cmd) => cmd.run(),
            Download(cmd) => cmd.run(),
            Generate(cmd) => cmd.run(),
            ZebradCmd::Help(cmd) => cmd.run(),
            Start(cmd) => cmd.run(),
            TipHeight(cmd) => cmd.run(),
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
