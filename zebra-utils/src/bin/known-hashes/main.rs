//! Generates the known-hash list pins for v2 checkpoint sync.
//!
//! Sweeps the `get-hashes` entry of every block (hash, size, and transaction,
//! transparent output and note commitment counts) from a local, fully synced
//! node over JSON-RPC, verifies the hashes against the network's spaced
//! checkpoint list (every checkpointed height must match exactly) and that
//! each block's parent is the swept block one height below, and
//! prints the `KnownHashListSpec` constant block: the SHA-256 pin of each
//! 50,000-height chunk's canonical `get-hashes` body.
//!
//! Typical usage, against a synced local node:
//!
//! ```sh
//! known-hashes --network mainnet --out-dir sweep/ --end-height 3358431
//! known-hashes --network mainnet --out-dir sweep/ --end-height 3358431 --emit-pins
//! ```
//!
//! For all options please refer to the program help: `known-hashes --help`

use clap::Parser;
use color_eyre::{
    eyre::{ensure, eyre, Result},
    Help,
};

use zebra_chain::parameters::Network;
use zebra_utils::init_tracing;

mod args;
mod emit;
mod source;
mod sweep;

#[cfg(test)]
mod tests;

use args::Args;
use source::{BlockSource, RpcBlockSource};
use sweep::{verify_anchors, verify_links, SweepFile};

/// The number of blocks below the node tip that the sweep stops at by
/// default, so it never records blocks that could still be reorged away.
const CONFIRMATION_MARGIN: u32 = 100;

/// How far below its tip a Zebra node serves `get-hashes` entries:
/// `zebra_state::MAX_BLOCK_REORG_HEIGHT`, which this tool does not depend on.
const SERVED_TIP_MARGIN: u32 = 1_000;

/// Process entry point for `known-hashes`
#[tokio::main]
// This is an interactive maintenance tool: results go to stdout, and progress
// and warnings go to stderr, following the other `zebra-utils` binaries.
#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let args = Args::parse();
    let network = &args.network;
    let network_name = network.to_string().to_lowercase();

    std::fs::create_dir_all(&args.out_dir)?;
    let sweep_path = args
        .out_dir
        .join(format!("known-hashes-sweep-{network_name}.bin"));

    let mut file = SweepFile::open(&sweep_path)?;

    if args.emit_pins {
        let max_height = args
            .end_height
            .ok_or_else(|| eyre!("--emit-pins requires --end-height"))
            .with_suggestion(|| {
                "the emitted `max_height` is a reviewed constant: \
                 pass the highest height the published list should cover"
            })?;

        let constant_block = run_emit(network, &file, max_height.0)?;

        // The pins are computed from the sweep alone, so the node is only asked for its tip, to
        // warn about a list that nodes cannot serve yet.
        let source = RpcBlockSource::new(args.rpc_addr);
        match source.tip_height().await {
            Ok(tip) => {
                if let Some(warning) = tip_margin_warning(max_height.0, tip) {
                    eprintln!("warning: {warning}");
                }
            }
            Err(error) => eprintln!(
                "could not read the node tip to check that peers can serve the list: {error}"
            ),
        }

        println!("{constant_block}");
    } else {
        let source = RpcBlockSource::new(args.rpc_addr);

        let end_height = match args.end_height {
            Some(height) => height.0,
            None => {
                let tip = source.tip_height().await.with_suggestion(|| {
                    "is the node running, and is --rpc-addr its JSON-RPC address?"
                })?;

                tip.checked_sub(CONFIRMATION_MARGIN).ok_or_else(|| {
                    eyre!(
                        "node tip {tip} is below the {CONFIRMATION_MARGIN} block \
                         confirmation margin: wait for the node to sync further",
                    )
                })?
            }
        };

        sweep::run_sweep(
            &source,
            network,
            &mut file,
            args.start_height.0,
            end_height,
            args.concurrency,
        )
        .await?;
    }

    Ok(())
}

/// A warning if a list ending at `max_height` is within [`SERVED_TIP_MARGIN`]
/// of the node tip `tip`.
///
/// Zebra nodes serve `get-hashes` entries only up to their tip minus
/// [`SERVED_TIP_MARGIN`], so until the chain is that far above `max_height`,
/// no node can serve the last chunk, and nodes that use the list fall back to
/// checkpoint anchors for it. This only matters on Regtest and custom testnets,
/// whose final checkpoint can be close to the tip.
fn tip_margin_warning(max_height: u32, tip: u32) -> Option<String> {
    let served_through = tip.checked_sub(SERVED_TIP_MARGIN);
    if served_through.is_some_and(|served_through| max_height <= served_through) {
        return None;
    }

    Some(format!(
        "--end-height {max_height} is within {SERVED_TIP_MARGIN} blocks of the node tip {tip}: \
         nodes serve known hashes only {SERVED_TIP_MARGIN} blocks below their tips, \
         so no node can serve the end of this list until the chain is longer"
    ))
}

/// Computes the chunk pins of the completed sweep in `file`, and returns the
/// `KnownHashListSpec` constant block.
///
/// Fails if `max_height` is above the network's final checkpoint, if any
/// height up to `max_height` is missing from the sweep, if the genesis hash
/// is wrong, if any spaced checkpoint anchor mismatches, or if any swept
/// block is not the child of the swept block one height below it.
//
// Progress reporting goes to stderr because this is an interactive
// maintenance tool, and `init_tracing()` does not install a format layer.
#[allow(clippy::print_stderr)]
fn run_emit(network: &Network, file: &SweepFile, max_height: u32) -> Result<String> {
    let final_checkpoint = network.checkpoint_list().max_height();
    ensure!(
        max_height <= final_checkpoint.0,
        "--end-height {max_height} is above the {network} final checkpoint {}: \
         a known-hash list never extends past the checkpoint list",
        final_checkpoint.0,
    );

    let entries = file.complete_entries(max_height)?;

    // Anchor verification: with completeness already checked, every spaced
    // checkpoint up to `max_height` must be present and match.
    let (verified, missing) = verify_anchors(network, file, max_height)?;
    ensure!(
        missing == 0,
        "{missing} checkpoint anchors at or below height {max_height} are not swept yet",
    );

    // Link verification: every hash between two anchors must be chained to
    // them through the swept parent hashes.
    let links = verify_links(file, max_height)?;

    let pins = emit::chunk_pins(network, &entries)?;
    emit::self_check(max_height, &pins, network, &entries)?;

    eprintln!(
        "pinned {} chunks covering {} heights \
         ({verified} anchors and {links} parent links verified)",
        pins.len(),
        entries.len(),
    );

    let const_prefix = match network {
        Network::Mainnet => "MAINNET",
        Network::Testnet(_) => "TESTNET",
    };

    Ok(emit::spec_constant_block(const_prefix, max_height, &pins))
}
