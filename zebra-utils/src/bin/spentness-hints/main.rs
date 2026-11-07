//! Generates and audits spentness-hint artifacts from a fully validated archive state.
//!
//! A spentness-hint artifact holds one bit per transparent output created at heights
//! `0..=H`, in canonical order: whether the output is still unspent at `H`. Nodes trust
//! an artifact only through the `MaxCheckpoint` pin (height, hash and SHA-256) that a
//! release adds to `zebra-chain`.
//!
//! - `replay`: copy a fully validated archive state into a separate state that ends
//!   exactly at `H`.
//! - `generate`: merge the canonical output order with the exact-`H` UTXO set into the
//!   artifact, and print the values a release pins.
//! - `verify`: authenticate an artifact against its pin, then check every bit against an
//!   independent transparent replay and the ordinary UTXO set.
//!
//! A release pins an artifact only after two independent generations from separately
//! validated archives give identical bytes, and `verify` accepts it.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod generate;
mod replay;
mod verify;

use std::{
    collections::HashSet,
    num::NonZeroUsize,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::{ensure, eyre, Result};
use zebra_chain::{
    block::{self, merkle, Block, Height},
    common::atomic_write,
    parameters::Network,
    transaction,
};
use zebra_state::{Config, ZebraDb};

/// The most threads that read and deserialize blocks ahead of a pass.
const MAX_BLOCK_READERS: usize = 8;

/// Blocks each reader may hold before the pass consumes them.
const BLOCKS_READ_AHEAD: usize = 16;

/// Log pass progress every this many blocks.
const PROGRESS_INTERVAL: u32 = 10_000;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// The network of the archive states: `mainnet` or `testnet`.
    #[arg(long, default_value = "mainnet")]
    network: Network,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build or advance a separate archive state that ends exactly at H.
    ///
    /// The source must be a state that Zebra fully validated. The source is never
    /// modified.
    Replay {
        /// The `cache_dir` of the fully validated source state.
        #[arg(long)]
        source: PathBuf,
        /// The `cache_dir` of the destination state, which must differ from the source.
        #[arg(long)]
        destination: PathBuf,
        /// The terminal height H.
        #[arg(long)]
        height: u32,
        /// The hash of the block at H.
        #[arg(long)]
        block_hash: block::Hash,
    },

    /// Generate the artifact from a state whose tip is exactly H.
    Generate {
        /// The `cache_dir` of the state, whose tip must be H.
        #[arg(long)]
        state: PathBuf,
        /// The terminal height H.
        #[arg(long)]
        height: u32,
        /// The hash of the block at H.
        #[arg(long)]
        block_hash: block::Hash,
        /// Where to write the artifact.
        #[arg(long)]
        output: PathBuf,
    },

    /// Check an artifact against its pin and an independent transparent replay.
    Verify {
        /// The `cache_dir` of a state whose tip is the artifact's terminal block.
        #[arg(long)]
        state: PathBuf,
        /// The artifact file.
        #[arg(long)]
        artifact: PathBuf,
        /// The pinned SHA-256 of the artifact, in hex. Without it, the pin built into
        /// this binary for the network is used.
        #[arg(long, requires_all = ["height", "block_hash"])]
        sha256: Option<String>,
        /// The pinned terminal height.
        #[arg(long, requires = "sha256")]
        height: Option<u32>,
        /// The pinned terminal block hash.
        #[arg(long, requires = "sha256")]
        block_hash: Option<block::Hash>,
    },
}

/// Archive state settings: always open the existing database, and never delete
/// another version.
fn state_config(path: &Path) -> Config {
    Config {
        cache_dir: path.to_owned(),
        delete_old_database: false,
        ..Config::default()
    }
}

/// Opens the archive state at `path` read-only.
fn open(path: &Path, network: &Network) -> Result<ZebraDb> {
    let (_, db, _) = zebra_state::init_read_only(state_config(path), network)?;
    Ok(db)
}

/// Requires the archive tip to equal `height`/`hash`, and returns the archive's genesis
/// hash, after checking that it is `network`'s.
fn exact_boundary(
    db: &ZebraDb,
    network: &Network,
    height: Height,
    hash: block::Hash,
) -> Result<block::Hash> {
    ensure!(
        db.tip() == Some((height, hash)),
        "archive tip must equal the requested height and hash"
    );
    let genesis = db
        .hash(Height(0))
        .ok_or_else(|| eyre!("archive has no genesis block"))?;
    ensure!(
        genesis == network.genesis_hash(),
        "archive chain identity differs from {network}"
    );
    Ok(genesis)
}

/// A canonical block read from the archive, and its transaction hashes.
struct CanonicalBlock {
    height: Height,
    block: Arc<Block>,
    tx_hashes: Vec<transaction::Hash>,
}

/// Reads the block at `height`, and checks that it is the archive's canonical block.
///
/// The Merkle root check binds the transactions to the checked header.
fn canonical_block(db: &ZebraDb, height: Height) -> Result<CanonicalBlock> {
    let block = db
        .block(height.into())
        .map_err(|error| eyre!("could not read block {}: {error}", height.0))?
        .ok_or_else(|| eyre!("archive lacks block {}", height.0))?;
    ensure!(
        db.hash(height) == Some(block.hash()),
        "canonical hash mismatch at {}",
        height.0
    );
    let tx_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
    ensure!(
        !tx_hashes.is_empty()
            && block.header.merkle_root == tx_hashes.iter().copied().collect::<merkle::Root>(),
        "transaction Merkle root mismatch at {}",
        height.0
    );
    // Duplicate transactions can reproduce a Merkle root (CVE-2012-2459).
    ensure!(
        tx_hashes.iter().collect::<HashSet<_>>().len() == tx_hashes.len(),
        "duplicate transaction at {}",
        height.0
    );
    Ok(CanonicalBlock {
        height,
        block,
        tx_hashes,
    })
}

/// Passes each canonical block in `heights` to `visit`, in height order.
///
/// Worker threads read, deserialize and check blocks ahead of `visit`, and send them
/// over bounded channels. `label` names the pass in progress logs.
fn for_each_block(
    db: &ZebraDb,
    heights: RangeInclusive<u32>,
    label: &str,
    mut visit: impl FnMut(CanonicalBlock) -> Result<()>,
) -> Result<()> {
    let readers = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MAX_BLOCK_READERS);
    let end = *heights.end();
    thread::scope(|scope| {
        // Reader `i` reads every `readers`-th height, starting at offset `i`.
        let receivers: Vec<_> = (0..readers)
            .map(|reader| {
                let (sender, receiver) = mpsc::sync_channel(BLOCKS_READ_AHEAD);
                let assigned = heights.clone().skip(reader).step_by(readers);
                scope.spawn(move || {
                    for height in assigned {
                        // A closed channel means the pass has stopped.
                        if sender.send(canonical_block(db, Height(height))).is_err() {
                            break;
                        }
                    }
                });
                receiver
            })
            .collect();
        for (height, receiver) in heights.clone().zip(receivers.iter().cycle()) {
            let block = receiver
                .recv()
                .map_err(|_| eyre!("block reader stopped before {height}"))??;
            visit(block)?;
            if height.is_multiple_of(PROGRESS_INTERVAL) {
                eprintln!("{label} {height}/{end}");
            }
        }
        Ok(())
    })
}

/// Writes `bytes` to `path` atomically.
fn write(path: PathBuf, bytes: &[u8]) -> Result<()> {
    atomic_write(path, bytes)??;
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let network = args.network;
    match args.command {
        Command::Replay {
            source,
            destination,
            height,
            block_hash,
        } => replay::replay(&network, &source, &destination, Height(height), block_hash),
        Command::Generate {
            state,
            height,
            block_hash,
            output,
        } => generate::run(&network, &state, Height(height), block_hash, output),
        Command::Verify {
            state,
            artifact,
            sha256,
            height,
            block_hash,
        } => {
            let pin = match (sha256, height, block_hash) {
                (Some(sha256), Some(height), Some(hash)) => {
                    Some(verify::pin_from_args(&sha256, Height(height), hash)?)
                }
                _ => None,
            };
            verify::run(&network, &state, &artifact, pin)
        }
    }
}

fn main() -> Result<()> {
    zebra_utils::init_tracing();
    color_eyre::install()?;
    run(Args::parse())
}

#[cfg(test)]
mod tests;
