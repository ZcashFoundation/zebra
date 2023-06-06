//! Zebrad EntryPoint

use std::cmp::min;

use abscissa_core::{Command, Configurable, FrameworkError, Runnable};
use clap::Parser;
use std::{ffi::OsString, path::PathBuf};

use crate::config::ZebradConfig;

use super::ZebradCmd;

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

    /// Process command arguments and insert the default subcommand
    /// if no subcommand is provided.
    pub fn process_cli_args(mut args: Vec<OsString>) -> Vec<OsString> {
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

            num_top_level_args = min(num_top_level_args, args.len());
            args.insert(num_top_level_args, EntryPoint::default_cmd_as_str().into());
        }

        args
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
