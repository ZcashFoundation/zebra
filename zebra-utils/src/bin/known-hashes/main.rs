//! Assembles and verifies known-hash list assets for known-hash IBD.
//!
//! Sweeps every block hash and serialized block size from a local, fully
//! synced node over JSON-RPC, verifies the result against the network's
//! spaced checkpoint list (every checkpointed height must match exactly), and
//! emits the chunked assets (per-block hashes with embedded size hints) and
//! the `KnownHashListSpec` constant block used by the known-hash IBD engine.
//!
//! Typical usage, against a synced local node:
//!
//! ```sh
//! known-hashes --network mainnet --out-dir assets/
//! known-hashes --network mainnet --out-dir assets/ --emit-chunks --end-height 3358431
//! ```
//!
//! For all options please refer to the program help: `known-hashes --help`

use color_eyre::{
    eyre::{ensure, eyre, Result},
    Help,
};
use structopt::StructOpt;

use zebra_chain::{block, parameters::Network};
use zebra_utils::init_tracing;

mod args;
mod emit;
mod source;
mod sweep;

#[cfg(test)]
mod tests;

use args::Args;
use source::{BlockSource, RpcBlockSource};
use sweep::{verify_anchors, SweepFiles};

/// The number of blocks below the node tip that the sweep stops at by
/// default, so it never records blocks that could still be reorged away.
const CONFIRMATION_MARGIN: u32 = 100;

/// Process entry point for `known-hashes`
#[tokio::main]
// This is an interactive maintenance tool: results go to stdout, and progress
// goes to stderr, following the other `zebra-utils` binaries.
#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let args = Args::from_args();
    let network = &args.network;

    // The emitted asset file name prefix (the spec's `file_prefix` field);
    // the network part matches the `main-checkpoints.txt`/
    // `test-checkpoints.txt` convention.
    let file_prefix = match network {
        Network::Mainnet => "main-known-hashes",
        Network::Testnet(_) => "test-known-hashes",
    };
    let network_name = network.to_string().to_lowercase();

    std::fs::create_dir_all(&args.out_dir)?;
    let hash_path = args
        .out_dir
        .join(format!("swept-hashes-{network_name}.bin"));
    let sizes_path = args.sizes_file.clone().unwrap_or_else(|| {
        args.out_dir
            .join(format!("block-sizes-{network_name}.u32le"))
    });

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;

    if args.emit_chunks {
        let max_height = args
            .end_height
            .ok_or_else(|| eyre!("--emit-chunks requires --end-height"))
            .with_suggestion(|| {
                "the emitted `max_height` is a reviewed constant: \
                 pass the highest height the published list should cover"
            })?;

        let constant_block = run_emit(network, &files, max_height.0, &args, file_prefix)?;
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
            &mut files,
            args.start_height.0,
            end_height,
            args.concurrency,
        )
        .await?;
    }

    Ok(())
}

/// Converts the completed sweep in `files` into the chunked assets and
/// returns the `KnownHashListSpec` constant block.
///
/// Fails if any height up to `max_height` is missing from the sweep, if the
/// genesis hash is wrong, or if any spaced checkpoint anchor mismatches.
//
// Progress reporting goes to stderr because this is an interactive
// maintenance tool, and `init_tracing()` does not install a format layer.
#[allow(clippy::print_stderr)]
fn run_emit(
    network: &Network,
    files: &SweepFiles,
    max_height: u32,
    args: &Args,
    file_prefix: &str,
) -> Result<String> {
    let hashes = files.complete_hashes(max_height)?;

    ensure!(
        block::Hash(hashes[0]) == network.genesis_hash(),
        "the swept hash at height 0 is {}, \
         but the {network} genesis hash is {}; \
         the sweep file is corrupted or for another network",
        block::Hash(hashes[0]),
        network.genesis_hash(),
    );

    // Anchor verification: with completeness already checked, every spaced
    // checkpoint up to `max_height` must be present and match.
    let (verified, missing) = verify_anchors(network, files, max_height)?;
    ensure!(
        missing == 0,
        "{missing} checkpoint anchors at or below height {max_height} are not swept yet",
    );

    let sizes = files.complete_sizes(max_height)?;
    let hints = sizes
        .iter()
        .enumerate()
        .map(|(height, size)| {
            emit::size_hint(*size)
                .map_err(|err| eyre!("invalid block size at height {height}: {err}"))
        })
        .collect::<Result<Vec<u8>>>()?;

    let spec = emit::emit_assets(&args.out_dir, file_prefix, &hashes, &hints)?;

    eprintln!(
        "emitted {} chunks with {} embedded size hints to {} ({verified} anchors verified)",
        spec.chunk_hashes.len(),
        hints.len(),
        args.out_dir.display(),
    );

    let const_prefix = match network {
        Network::Mainnet => "MAINNET",
        Network::Testnet(_) => "TESTNET",
    };

    Ok(emit::spec_constant_block(
        const_prefix,
        file_prefix,
        max_height,
        &spec,
    ))
}
