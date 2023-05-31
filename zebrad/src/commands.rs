//! Zebrad Subcommands

mod copy_state;
mod download;
mod generate;
mod start;
mod tip_height;

use self::ZebradCmd::*;
use self::{
    copy_state::CopyStateCmd, download::DownloadCmd, generate::GenerateCmd,
    tip_height::TipHeightCmd,
};

pub use self::start::StartCmd;

use crate::config::ZebradConfig;

use abscissa_core::{config::Override, Command, Configurable, FrameworkError, Runnable};
use clap::Parser;
use std::ffi::OsString;
use std::path::PathBuf;

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

/// Zebrad Subcommands
#[derive(Command, Debug, clap::Subcommand)]
pub enum ZebradCmd {
    /// The `copy-state` subcommand, used to debug cached chain state
    // TODO: hide this command from users in release builds (#3279)
    CopyState(CopyStateCmd),

    // The `download` subcommand
    /// pre-download required parameter files
    Download(DownloadCmd),

    // The `generate` subcommand
    /// generate a skeleton configuration
    Generate(GenerateCmd),

    // The `start` subcommand
    /// start the application
    Start(StartCmd),

    // The `tip-height` subcommand
    /// get the block height of Zebra's persisted chain state
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
            Download(_) | Generate(_) | TipHeight(_) => false,
        }
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
            CopyState(_) | Download(_) | Start(_) => false,
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
            Download(cmd) => cmd.run(),
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

/// Toplevel entrypoint command.
///
/// Handles obtaining toplevel help as well as verbosity settings.
#[derive(Debug, clap::Parser)]
#[clap(
    version = clap::crate_version!(), 
    author="Zcash Foundation <zebra@zfnd.org>",
    help_template = "\
{name} {version}\n
{author}\n
{usage-heading} {usage}\n
{all-args}\
"
)]
pub struct EntryPoint {
    /// Subcommand to execute.
    ///
    /// The `command` option will delegate option parsing to the command type,
    /// starting at the first free argument. Defaults to start.
    #[clap(subcommand)]
    pub cmd: Option<ZebradCmd>,

    /// Path to the configuration file
    #[clap(long, short, help = "path to configuration file")]
    pub config: Option<PathBuf>,

    /// Increase verbosity setting
    #[clap(long, short, help = "be verbose")]
    pub verbose: bool,

    /// Filter strings which override the config file and defaults
    // This can be applied to the default start command if no subcommand is provided.
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,
}

impl EntryPoint {
    /// Borrow the command in the option
    ///
    /// # Panics
    ///
    /// If `cmd` is None
    pub fn cmd(&self) -> &ZebradCmd {
        self.cmd
            .as_ref()
            .expect("should default to start if not provided")
    }

    /// Returns a string that parses to the default subcommand
    pub fn default_cmd_as_str() -> &'static str {
        "start"
    }

    /// Process arguments from [`env::args_os()`] and insert the default subcommand
    /// if no subcommand is provided.
    pub fn process_cli_args(args: Vec<OsString>) -> Vec<OsString> {
        // Check if the provided arguments include a subcommand
        let should_add_default_subcommand = EntryPoint::try_parse_from(&args)
            .unwrap_or_else(|err| err.exit())
            .cmd
            .is_none();

        // Add the default subcommand to args after the top-level args if cmd is None
        if should_add_default_subcommand {
            // try_parse_from currently produces an error if the first argument is not the binary name,
            let mut num_top_level_args = 1;

            // update last_top_level_arg_idx to the number of top-level args
            for (idx, arg) in args.iter().enumerate() {
                num_top_level_args = match arg.to_str() {
                    Some("--verbose" | "-v" | "--version" | "-V" | "--help") => idx + 1,
                    Some("--config" | "-c") => idx + 2,
                    _ => num_top_level_args,
                }
            }

            let mut updated_args: Vec<_> = args.iter().take(num_top_level_args).cloned().collect();
            updated_args.push(EntryPoint::default_cmd_as_str().into());
            updated_args.extend(args.into_iter().skip(num_top_level_args));

            updated_args
        } else {
            args
        }
    }
}

impl Runnable for EntryPoint {
    fn run(&self) {
        self.cmd().run()
    }
}

impl Command for EntryPoint {
    /// Name of this program as a string
    fn name() -> &'static str {
        ZebradCmd::name()
    }

    /// Description of this program
    fn description() -> &'static str {
        ZebradCmd::description()
    }

    /// Authors of this program
    fn authors() -> &'static str {
        ZebradCmd::authors()
    }
}

impl Configurable<ZebradConfig> for EntryPoint {
    /// Path to the command's configuration file
    fn config_path(&self) -> Option<PathBuf> {
        match &self.config {
            // Use explicit `-c`/`--config` argument if passed
            Some(cfg) => Some(cfg.clone()),

            // Otherwise defer to the toplevel command's config path logic
            None => self.cmd().config_path(),
        }
    }

    /// Process the configuration after it has been loaded, potentially
    /// modifying it or returning an error if options are incompatible
    fn process_config(&self, config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        self.cmd().process_config(config)
    }
}
