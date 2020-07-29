//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

#![deny(missing_docs)]
#![allow(clippy::try_err)]

use color_eyre::eyre::{ensure, Result};
use serde_json::Value;
use std::process::Stdio;
use structopt::StructOpt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::types::BlockHeight;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

mod args;

/// We limit the memory usage for each checkpoint, based on the cumulative size of
/// the serialized blocks in the chain. Deserialized blocks are larger, because
/// they contain pointers and non-compact integers. But they should be within a
/// constant factor of the serialized size.
const MAX_CHECKPOINT_BYTE_COUNT: u64 = 256 * 1024 * 1024;

/// Checkpoints must be on the main chain, so we skip blocks that are within the
/// zcashd reorg limit.
const BLOCK_REORG_LIMIT: BlockHeight = BlockHeight(100);

/// Initialise tracing using its defaults.
fn init_tracing() {
    tracing_subscriber::Registry::default()
        .with(tracing_error::ErrorLayer::default())
        .init();
}

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

fn main() -> Result<()> {
    init_tracing();

    color_eyre::install()?;

    // get the current block count
    let mut cmd = passthrough_cmd();
    cmd.arg("getblockcount");
    // calculate the maximum height
    let height_limit: BlockHeight = cmd_output(&mut cmd)?.trim().parse()?;
    let height_limit = height_limit
        .0
        .checked_sub(BLOCK_REORG_LIMIT.0)
        .map(BlockHeight)
        .expect("zcashd has some mature blocks: wait for zcashd to sync more blocks");

    let mut starting_height = BlockHeight(args::Args::from_args().last_checkpoint);
    if starting_height.0 > 0 {
        starting_height.0 += 1;
    }
    assert!(starting_height < height_limit);

    // set up counters
    let mut cumulative_bytes: u64 = 0;
    let mut height_gap: BlockHeight = BlockHeight(0);

    // loop through all blocks
    for x in starting_height.0..height_limit.0 {
        // unfortunatly we need to create a process for each block
        let mut cmd = passthrough_cmd();

        // get block data
        cmd.args(&["getblock", &x.to_string()]);
        let output = cmd_output(&mut cmd)?;
        // parse json
        let v: Value = serde_json::from_str(&output)?;

        // get the values we are interested in
        let hash: BlockHeaderHash = v["hash"]
            .as_str()
            .map(zebra_chain::utils::byte_reverse_hex)
            .unwrap()
            .parse()?;
        let height = BlockHeight(v["height"].as_u64().unwrap() as u32);
        assert!(height <= BlockHeight::MAX);
        assert_eq!(x, height.0);
        let size = v["size"].as_u64().unwrap();
        assert!(size <= zebra_chain::block::MAX_BLOCK_BYTES);

        // compute
        cumulative_bytes += size;
        height_gap = BlockHeight(height_gap.0 + 1);

        // check if checkpoint
        if height == BlockHeight(0)
            || cumulative_bytes >= MAX_CHECKPOINT_BYTE_COUNT
            || height_gap.0 >= zebra_consensus::checkpoint::MAX_CHECKPOINT_HEIGHT_GAP as u32
        {
            // print to output
            println!("{} {}", height.0, &hex::encode(hash.0),);

            // reset counters
            cumulative_bytes = 0;
            height_gap = BlockHeight(0);
        }
    }

    Ok(())
}
