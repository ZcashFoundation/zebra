//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! Usage: zebra-checkpoints [network] --cli <cli-path>
//! Where network can be `--regtest`, `mainnet` or `--testnet`.
//! `--cli` is the path of the zcash-cli binary as a string.
//! If no network is specified program will use the mainnet.
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

#![allow(clippy::try_err)]
use color_eyre::eyre::Result;
use serde_json::Value;
use std::process::Stdio;
use structopt::StructOpt;

mod args;

/// We limit the maximum number of blocks in each checkpoint. Each block uses a
/// constant amount of memory for the supporting data structures and futures.
const MAX_CHECKPOINT_HEIGHT_GAP: usize = zebrad::commands::LOOKAHEAD_LIMIT / 2;

/// We limit the memory usage for each checkpoint, based on the cumulative size of
/// the serialized blocks in the chain. Deserialized blocks are larger, because
/// they contain pointers and non-compact integers. But they should be within a
/// constant factor of the serialized size.
const MAX_CHECKPOINT_BYTE_COUNT: i64 = 256 * 1024 * 1024;

/// Checkpoints must be on the main chain, so we skip blocks that are within the
/// zcashd reorg limit.
const BLOCK_REORG_LIMIT: i64 = 100;

// Add network argument if needed
fn network(mut cmd: std::process::Command, args: &args::Args) -> std::process::Command {
    if args.testnet {
        cmd.arg("-testnet");
    } else if args.regtest {
        cmd.arg("-regtest");
    }
    cmd
}

fn main() -> Result<()> {
    // todo add tracing setup

    color_eyre::install()?;

    // create process
    let args = args::Args::from_args();
    let mut cmd = std::process::Command::new(&args.cli);
    cmd = network(cmd, &args);

    // set up counters
    let mut cumulative_bytes: i64 = 0;
    let mut height_gap: usize = 0;

    // get the current block count
    cmd.arg("getblockcount");
    cmd.stdout(Stdio::piped()).spawn().unwrap();
    let output = cmd.output().unwrap();
    let mut block_count: i64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    block_count -= BLOCK_REORG_LIMIT;

    // loop through all blocks
    for x in 0..block_count {
        // unfortunatly we need to create a process for each block
        let mut cmd = std::process::Command::new(&args.cli);
        cmd = network(cmd, &args);

        // get block data
        cmd.args(&["getblock", &x.to_string()]);
        cmd.stdout(Stdio::piped()).spawn().unwrap();
        let output = cmd.output().unwrap();
        let block_raw = String::from_utf8_lossy(&output.stdout);

        // convert raw block to json
        let v: Value = serde_json::from_str(block_raw.trim())?;

        // get the values we are interested in
        let hash = &v["hash"].as_str().unwrap();
        let height = &v["height"].as_i64().unwrap();
        let size = &v["size"].as_i64().unwrap();

        // compute
        cumulative_bytes += size;
        height_gap += 1;

        // check if checkpoint
        if *height == 0
            || cumulative_bytes >= MAX_CHECKPOINT_BYTE_COUNT
            || height_gap >= MAX_CHECKPOINT_HEIGHT_GAP
        {
            // print to output
            println!("{} {}", height, zebra_chain::utils::byte_reverse_hex(hash));

            // reset counters
            cumulative_bytes = 0;
            height_gap = 0;
        }
    }

    Ok(())
}
