//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

use color_eyre::eyre::{ensure, Result};
use serde_json::Value;
use std::process::Stdio;
use structopt::StructOpt;

use zebra_chain::block;
use zebra_utils::init_tracing;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

mod args;

/// Return a new `zcash-cli` command, including the `zebra-checkpoints`
/// passthrough arguments.
fn passthrough_cmd() -> std::process::Command {
    let args = args::Args::from_args();
    let mut cmd = std::process::Command::new(&args.cli);

    if !args.zcli_args.is_empty() {
        cmd.args(&args.zcli_args);
    }
    cmd
}

/// Run `cmd` and return its output as a string.
fn cmd_output(cmd: &mut std::process::Command) -> Result<String> {
    // Capture stdout, but send stderr to the user
    let output = cmd.stderr(Stdio::inherit()).output()?;

    // Make sure the command was successful
    #[cfg(unix)]
    ensure!(
        output.status.success(),
        "Process failed: exit status {:?}, signal: {:?}",
        output.status.code(),
        output.status.signal()
    );
    #[cfg(not(unix))]
    ensure!(
        output.status.success(),
        "Process failed: exit status {:?}",
        output.status.code()
    );

    // Make sure the output is valid UTF-8
    let s = String::from_utf8(output.stdout)?;
    Ok(s)
}

#[allow(clippy::print_stdout)]
fn main() -> Result<()> {
    init_tracing();

    color_eyre::install()?;

    // get the current block count
    let mut cmd = passthrough_cmd();
    cmd.arg("getblockcount");
    // calculate the maximum height
    let height_limit: block::Height = cmd_output(&mut cmd)?.trim().parse()?;
    assert!(height_limit <= block::Height::MAX);
    // Checkpoints must be on the main chain, so we skip blocks that are within the
    // Zcash reorg limit.
    let height_limit = height_limit
        .0
        .checked_sub(zebra_state::MAX_BLOCK_REORG_HEIGHT)
        .map(block::Height)
        .expect("zcashd has some mature blocks: wait for zcashd to sync more blocks");

    let starting_height = args::Args::from_args().last_checkpoint.map(block::Height);
    if starting_height.is_some() {
        // Since we're about to add 1, height needs to be strictly less than the maximum
        assert!(starting_height.unwrap() < block::Height::MAX);
    }
    // Start at the next block after the last checkpoint.
    // If there is no last checkpoint, start at genesis (height 0).
    let starting_height = starting_height.map_or(0, |block::Height(h)| h + 1);

    assert!(
        starting_height < height_limit.0,
        "No mature blocks after the last checkpoint: wait for zcashd to sync more blocks"
    );

    // set up counters
    let mut cumulative_bytes: u64 = 0;
    let mut height_gap: block::Height = block::Height(0);

    // loop through all blocks
    for x in starting_height..height_limit.0 {
        // unfortunately we need to create a process for each block
        let mut cmd = passthrough_cmd();

        // get block data
        cmd.args(&["getblock", &x.to_string()]);
        let output = cmd_output(&mut cmd)?;
        // parse json
        let v: Value = serde_json::from_str(&output)?;

        // get the values we are interested in
        let hash: block::Hash = v["hash"].as_str().unwrap().parse()?;
        let height = block::Height(v["height"].as_u64().unwrap() as u32);
        assert!(height <= block::Height::MAX);
        assert_eq!(x, height.0);
        let size = v["size"].as_u64().unwrap();

        // compute
        cumulative_bytes += size;
        height_gap = block::Height(height_gap.0 + 1);

        // check if checkpoint
        if height == block::Height(0)
            || cumulative_bytes >= zebra_consensus::MAX_CHECKPOINT_BYTE_COUNT
            || height_gap.0 >= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP as u32
        {
            // print to output
            println!("{} {}", height.0, hash);

            // reset counters
            cumulative_bytes = 0;
            height_gap = block::Height(0);
        }
    }

    Ok(())
}
