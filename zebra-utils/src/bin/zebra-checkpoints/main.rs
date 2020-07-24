//! Prints Zebra checkpoints as "height hash" output lines.
//!
//! Get all the blocks up to network current tip and print the ones that are
//! checkpoints according to rules.
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`
//!
//! zebra-consensus accepts an ordered list of checkpoints, starting with the
//! genesis block. Checkpoint heights can be chosen arbitrarily.

#![allow(clippy::try_err)]
use color_eyre::eyre::Result;
use serde_json::Value;
use std::process::Stdio;
use structopt::StructOpt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::types::BlockHeight;

mod args;

/// We limit the memory usage for each checkpoint, based on the cumulative size of
/// the serialized blocks in the chain. Deserialized blocks are larger, because
/// they contain pointers and non-compact integers. But they should be within a
/// constant factor of the serialized size.
const MAX_CHECKPOINT_BYTE_COUNT: u64 = 256 * 1024 * 1024;

/// Checkpoints must be on the main chain, so we skip blocks that are within the
/// zcashd reorg limit.
const BLOCK_REORG_LIMIT: BlockHeight = BlockHeight(100);

// Passthrough arguments if needed
fn passthrough(mut cmd: std::process::Command, args: &args::Args) -> std::process::Command {
    if !args.zcli_args.is_empty() {
        cmd.args(&args.zcli_args);
    }
    cmd
}

fn main() -> Result<()> {
    init_tracing();

    color_eyre::install()?;

    // create process
    let args = args::Args::from_args();
    let mut cmd = std::process::Command::new(&args.cli);
    cmd = passthrough(cmd, &args);

    // set up counters
    let mut cumulative_bytes: u64 = 0;
    let mut height_gap: BlockHeight = BlockHeight(0);

    // get the current block count
    cmd.arg("getblockcount");
    let mut subprocess = cmd.stdout(Stdio::piped()).spawn().unwrap();
    let output = cmd.output().unwrap();
    subprocess.kill()?;
    let mut requested_height: BlockHeight = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    requested_height = BlockHeight(
        requested_height
            .0
            .checked_sub(BLOCK_REORG_LIMIT.0)
            .expect("zcashd has some mature blocks: wait for zcashd to sync more blocks"),
    );

    // loop through all blocks
    for x in 0..requested_height.0 {
        // unfortunatly we need to create a process for each block
        let mut cmd = std::process::Command::new(&args.cli);
        cmd = passthrough(cmd, &args);

        // get block data
        cmd.args(&["getblock", &x.to_string()]);
        let mut subprocess = cmd.stdout(Stdio::piped()).spawn().unwrap();
        let output = cmd.output().unwrap();
        let block_raw = String::from_utf8_lossy(&output.stdout);

        // convert raw block to json
        let v: Value = serde_json::from_str(block_raw.trim())?;

        // get the values we are interested in
        let hash: BlockHeaderHash = v["hash"]
            .as_str()
            .map(zebra_chain::utils::byte_reverse_hex)
            .unwrap()
            .parse()
            .unwrap();
        let height = BlockHeight(v["height"].as_u64().unwrap() as u32);
        assert!(height <= BlockHeight::MAX);
        assert_eq!(x, height.0);
        let size = v["size"].as_u64().unwrap();
        assert!(size <= zebra_chain::block::MAX_BLOCK_BYTES);

        // kill spawned
        subprocess.wait()?;

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

fn init_tracing() {
    tracing_subscriber::Registry::default()
        .with(tracing_error::ErrorLayer::default())
        .init();
}
