//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

use std::{ffi::OsStr, process::Stdio};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use color_eyre::eyre::{ensure, Result};
use serde_json::Value;
use structopt::StructOpt;

use zebra_chain::{
    block::{self, Block, Height, HeightDiff, TryIntoHeight},
    serialization::ZcashDeserializeInto,
    transparent::MIN_TRANSPARENT_COINBASE_MATURITY,
};
use zebra_node_services::constants::{MAX_CHECKPOINT_BYTE_COUNT, MAX_CHECKPOINT_HEIGHT_GAP};
use zebra_utils::init_tracing;

pub mod args;

use args::{Args, Backend};

/// Run `cmd` with `cli_args` and `rpc_command`, and return its output as a string.
fn cli_output<S>(cli_args: &Args, rpc_command: &[S]) -> Result<String>
where
    S: AsRef<OsStr>,
{
    // Get a new `zcash-cli` command, including the `zebra-checkpoints` passthrough arguments.
    let mut cmd = std::process::Command::new(&cli_args.cli);
    cmd.args(&cli_args.zcli_args);

    // Add the RPC command and arguments
    cmd.args(rpc_command);

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

    let args = args::Args::from_args();

    // get the current block count
    let get_block_chain_info = cli_output(&args, &["getblockchaininfo"])?;
    let get_block_chain_info: Value = serde_json::from_str(&get_block_chain_info)?;

    // calculate the maximum height
    let height_limit = get_block_chain_info["blocks"]
        .try_into_height()
        .expect("height: unexpected invalid value, missing field, or field type");

    // Checkpoints must be on the main chain, so we skip blocks that are within the
    // Zcash reorg limit.
    let height_limit = height_limit
        - HeightDiff::try_from(MIN_TRANSPARENT_COINBASE_MATURITY).expect("constant fits in i32");
    let height_limit =
        height_limit.expect("node has some mature blocks: wait for it to sync more blocks");

    // Start at the next block after the last checkpoint.
    // If there is no last checkpoint, start at genesis (height 0).
    let starting_height = if let Some(last_checkpoint) = args.last_checkpoint {
        (last_checkpoint + 1)
            .expect("invalid last checkpoint height, must be less than the max height")
    } else {
        Height::MIN
    };

    assert!(
        starting_height < height_limit,
        "No mature blocks after the last checkpoint: wait for node to sync more blocks"
    );

    // set up counters
    let mut cumulative_bytes: u64 = 0;
    let mut last_checkpoint_height = args.last_checkpoint.unwrap_or(Height::MIN);
    let max_checkpoint_height_gap =
        HeightDiff::try_from(MAX_CHECKPOINT_HEIGHT_GAP).expect("constant fits in HeightDiff");

    // loop through all blocks
    for request_height in starting_height.0..height_limit.0 {
        // In `Cli` transport mode we need to create a process for each block

        let (hash, response_height, size) = match args.backend {
            Backend::Zcashd => {
                // get block data from zcashd using verbose=1
                let get_block = cli_output(&args, &["getblock", &request_height.to_string(), "1"])?;

                // parse json
                let get_block: Value = serde_json::from_str(&get_block)?;

                // get the values we are interested in
                let hash: block::Hash = get_block["hash"]
                    .as_str()
                    .expect("hash: unexpected missing field or field type")
                    .parse()?;
                let response_height: Height = get_block["height"]
                    .try_into_height()
                    .expect("height: unexpected invalid value, missing field, or field type");

                let size = get_block["size"]
                    .as_u64()
                    .expect("size: unexpected invalid value, missing field, or field type");

                (hash, response_height, size)
            }
            Backend::Zebrad => {
                // get block data from zebrad (or zcashd) by deserializing the raw block
                let block_bytes =
                    cli_output(&args, &["getblock", &request_height.to_string(), "0"])?;

                let block_bytes: Vec<u8> = hex::decode(block_bytes.trim_end_matches('\n'))?;

                // TODO: is it faster to call both `getblock height 0` and `getblock height 1`,
                //       rather than deserializing the block and calculating its hash?
                let block: Block = block_bytes.zcash_deserialize_into()?;

                (
                    block.hash(),
                    block
                        .coinbase_height()
                        .expect("block has always a coinbase height"),
                    block_bytes.len().try_into()?,
                )
            }
        };

        assert_eq!(
            request_height, response_height.0,
            "node returned a different block than requested"
        );

        // compute cumulative totals
        cumulative_bytes += size;

        let height_gap = response_height - last_checkpoint_height;

        // check if this block should be a checkpoint
        if response_height == Height::MIN
            || cumulative_bytes >= MAX_CHECKPOINT_BYTE_COUNT
            || height_gap >= max_checkpoint_height_gap
        {
            // print to output
            println!("{} {hash}", response_height.0);

            // reset cumulative totals
            cumulative_bytes = 0;
            last_checkpoint_height = response_height;
        }
    }

    Ok(())
}
