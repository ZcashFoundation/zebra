//! `version` subcommand

#![allow(clippy::never_loop)]

use super::ZebradCmd;
use abscissa_core::{Command, Options, Runnable};

/// `version` subcommand
#[derive(Command, Debug, Default, Options)]
pub struct VersionCmd {}

impl Runnable for VersionCmd {
    /// Print version message
    #[allow(clippy::print_stdout)]
    fn run(&self) {
        println!("{} {}", ZebradCmd::name(), ZebradCmd::version());
    }
}
