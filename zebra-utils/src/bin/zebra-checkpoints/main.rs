//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

use std::process::Stdio;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use color_eyre::eyre::{ensure, Result};
use hex::FromHex;
use serde_json::Value;
use structopt::StructOpt;

use zebra_chain::{
    block, serialization::ZcashDeserializeInto, transparent::MIN_TRANSPARENT_COINBASE_MATURITY,
};
use zebra_node_services::constants::{MAX_CHECKPOINT_BYTE_COUNT, MAX_CHECKPOINT_HEIGHT_GAP};
use zebra_utils::init_tracing;

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

/// Process entry point for `zebra-checkpoints`
#[allow(clippy::print_stdout)]
fn main() -> Result<()> {
    // initialise
    init_tracing();
    color_eyre::install()?;

    // get the current block count
    let mut cmd = passthrough_cmd();
    cmd.arg("getblockchaininfo");

    let output = cmd_output(&mut cmd)?;
    let get_block_chain_info: Value = serde_json::from_str(&output)?;

    // calculate the maximum height
    let height_limit = block::Height(get_block_chain_info["blocks"].as_u64().unwrap() as u32);

    assert!(height_limit <= block::Height::MAX);
    // Checkpoints must be on the main chain, so we skip blocks that are within the
    // Zcash reorg limit.
    let height_limit = height_limit
        .0
        .checked_sub(MIN_TRANSPARENT_COINBASE_MATURITY)
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

        let (hash, height, size) = match args::Args::from_args().backend {
            args::Backend::Zcashd => {
                // get block data from zcashd using verbose=1
                cmd.args(["getblock", &x.to_string(), "1"]);
                let output = cmd_output(&mut cmd)?;

                // parse json
                let v: Value = serde_json::from_str(&output)?;

                // get the values we are interested in
                let hash: block::Hash = v["hash"].as_str().unwrap().parse()?;
                let height = block::Height(v["height"].as_u64().unwrap() as u32);

                let size = v["size"].as_u64().unwrap();

                (hash, height, size)
            }
            args::Backend::Zebrad => {
                // get block data from zebrad by deserializing the raw block
                cmd.args(["getblock", &x.to_string(), "0"]);
                let output = cmd_output(&mut cmd)?;

                let block_bytes = <Vec<u8>>::from_hex(output.trim_end_matches('\n'))?;

                let block = block_bytes
                    .zcash_deserialize_into::<block::Block>()
                    .expect("obtained block should deserialize");

                (
                    block.hash(),
                    block
                        .coinbase_height()
                        .expect("block has always a coinbase height"),
                    block_bytes.len().try_into()?,
                )
            }
        };

        assert!(height <= block::Height::MAX);
        assert_eq!(x, height.0);

        // compute
        cumulative_bytes += size;
        height_gap = block::Height(height_gap.0 + 1);

        // check if checkpoint
        if height == block::Height(0)
            || cumulative_bytes >= MAX_CHECKPOINT_BYTE_COUNT
            || height_gap.0 >= MAX_CHECKPOINT_HEIGHT_GAP as u32
        {
            // print to output
            println!("{} {hash}", height.0);

            // reset counters
            cumulative_bytes = 0;
            height_gap = block::Height(0);
        }
    }

    Ok(())
}
