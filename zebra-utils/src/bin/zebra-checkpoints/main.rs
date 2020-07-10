#![allow(clippy::try_err)]
use color_eyre::eyre::{eyre, Result};
use structopt::StructOpt;

mod args;

fn main() -> Result<()> {
    // todo add tracing setup

    color_eyre::install()?;

    let args = args::Args::from_args();

    let mut cmd = std::process::Command::new("zcash-cli");

    if args.testnet {
        cmd.arg("-testnet");
    }

    cmd.args(args.zcli_args.into_iter());

    let mut child = cmd.spawn()?;

    // handle communicating with this child process via it's stdin and stdout handles

    let exit_status = child.wait()?;

    if !exit_status.success() {
        Err(eyre!("throw a more informative error here, might wanna shove stdin / stdout in here as custom sections"))?;
    }

    Ok(())
}
