//! Zebrad EntryPoint

use crate::{
    commands::{StartCmd, ZebradCmd},
    config::ZebradConfig,
};

use std::path::PathBuf;

use abscissa_core::{
    command::{Command, Usage},
    config::Configurable,
    FrameworkError, Options, Runnable,
};

// (See https://docs.rs/abscissa_core/0.5.2/src/abscissa_core/command/entrypoint.rs.html)
/// Toplevel entrypoint command.
///
/// Handles obtaining toplevel help as well as verbosity settings.
#[derive(Debug, Options)]
pub struct EntryPoint {
    /// Path to the configuration file
    #[options(short = "c", help = "path to configuration file")]
    pub config: Option<PathBuf>,

    /// Obtain help about the current command
    #[options(short = "h", help = "print help message")]
    pub help: bool,

    /// Increase verbosity setting
    #[options(short = "v", help = "be verbose")]
    pub verbose: bool,

    /// Subcommand to execute.
    ///
    /// The `command` option will delegate option parsing to the command type,
    /// starting at the first free argument. Defaults to start.
    #[options(command, default_expr = "Some(ZebradCmd::Start(StartCmd::default()))")]
    pub command: Option<ZebradCmd>,
}

impl EntryPoint {
    /// Borrow the underlying command type
    fn command(&self) -> &ZebradCmd {
        if self.help {
            let _ = Usage::for_command::<EntryPoint>().print_info();
            let _ = Usage::for_command::<EntryPoint>().print_usage();
            let _ = Usage::for_command::<ZebradCmd>().print_usage();
            std::process::exit(0);
        }

        self.command
            .as_ref()
            .expect("Some(ZebradCmd::Start(StartCmd::default()) as default value")
    }
}

impl Runnable for EntryPoint {
    fn run(&self) {
        self.command().run()
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

    /// Version of this program
    fn version() -> &'static str {
        ZebradCmd::version()
    }

    /// Authors of this program
    fn authors() -> &'static str {
        ZebradCmd::authors()
    }

    /// Get usage information for a particular subcommand (if available)
    fn subcommand_usage(command: &str) -> Option<Usage> {
        ZebradCmd::subcommand_usage(command)
    }
}

impl Configurable<ZebradConfig> for EntryPoint {
    /// Path to the command's configuration file
    fn config_path(&self) -> Option<PathBuf> {
        match &self.config {
            // Use explicit `-c`/`--config` argument if passed
            Some(cfg) => Some(cfg.clone()),

            // Otherwise defer to the toplevel command's config path logic
            None => self.command.as_ref().and_then(|cmd| cmd.config_path()),
        }
    }

    /// Process the configuration after it has been loaded, potentially
    /// modifying it or returning an error if options are incompatible
    fn process_config(&self, config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        match &self.command {
            Some(cmd) => cmd.process_config(config),
            None => Ok(config),
        }
    }
}
