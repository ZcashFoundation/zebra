//! Zebrad Subcommands

#![allow(non_local_definitions)]

use std::path::PathBuf;

use abscissa_core::{config::Override, Command, Configurable, FrameworkError, Runnable};

use crate::config::ZebradConfig;

pub use self::{entry_point::EntryPoint, start::StartCmd};

use self::{copy_state::CopyStateCmd, generate::GenerateCmd, tip_height::TipHeightCmd};

pub mod start;

mod copy_state;
mod entry_point;
mod generate;
mod tip_height;

#[cfg(test)]
mod tests;

use ZebradCmd::*;

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

/// Zebrad Subcommands
#[derive(Command, Debug, clap::Subcommand)]
pub enum ZebradCmd {
    /// The `copy-state` subcommand, used to debug cached chain state (expert users only)
    // TODO: hide this command from users in release builds (#3279)
    CopyState(CopyStateCmd),

    /// Generate a default `zebrad.toml` configuration
    Generate(GenerateCmd),

    /// Start the application (default command)
    Start(StartCmd),

    /// Print the tip block height of Zebra's chain state on disk
    TipHeight(TipHeightCmd),
}

impl ZebradCmd {
    /// Returns true if this command is a server command.
    ///
    /// Servers load extra components, and use the configured tracing filter.
    ///
    /// For example, `Start` acts as a Zcash node.
    pub(crate) fn is_server(&self) -> bool {
        // List all the commands, so new commands have to make a choice here
        match self {
            // Commands that run as a configured server
            CopyState(_) | Start(_) => true,

            // Utility commands that don't use server components
            Generate(_) | TipHeight(_) => false,
        }
    }

    /// Returns true if this command shows the Zebra intro logo and text.
    ///
    /// For example, `Start` acts as a Zcash node.
    pub(crate) fn uses_intro(&self) -> bool {
        // List all the commands, so new commands have to make a choice here
        match self {
            // Commands that need an intro
            Start(_) => true,

            // Utility commands
            CopyState(_) | Generate(_) | TipHeight(_) => false,
        }
    }

    /// Returns true if this command should ignore errors when
    /// attempting to load a config file.
    pub(crate) fn should_ignore_load_config_error(&self) -> bool {
        matches!(self, ZebradCmd::Generate(_))
    }

    /// Returns the default log level for this command, based on the `verbose` command line flag.
    ///
    /// Some commands need to be quiet by default.
    pub(crate) fn default_tracing_filter(&self, verbose: bool) -> &'static str {
        let only_show_warnings = match self {
            // Commands that generate quiet output by default.
            // This output:
            // - is used by automated tools, or
            // - needs to be read easily.
            Generate(_) | TipHeight(_) => true,

            // Commands that generate informative logging output by default.
            CopyState(_) | Start(_) => false,
        };

        if only_show_warnings && !verbose {
            "warn"
        } else if only_show_warnings || !verbose {
            "info"
        } else {
            "debug"
        }
    }
}

impl Runnable for ZebradCmd {
    fn run(&self) {
        match self {
            CopyState(cmd) => cmd.run(),
            Generate(cmd) => cmd.run(),
            Start(cmd) => cmd.run(),
            TipHeight(cmd) => cmd.run(),
        }
    }
}

/// This trait allows you to define how application configuration is loaded.
impl Configurable<ZebradConfig> for ZebradCmd {
    /// Location of the configuration file
    fn config_path(&self) -> Option<PathBuf> {
        // Return the default config path if it exists, otherwise None
        // ZebradConfig::load() will handle file existence and fallback to defaults
        dirs::preference_dir().map(|path| path.join(CONFIG_FILE))

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
