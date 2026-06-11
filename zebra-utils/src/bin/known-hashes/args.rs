//! known-hashes arguments
//!
//! For usage please refer to the program help: `known-hashes --help`

use std::{net::SocketAddr, path::PathBuf};

use structopt::StructOpt;

use zebra_chain::{block::Height, parameters::Network};

/// known-hashes arguments
#[derive(Clone, Debug, StructOpt)]
pub struct Args {
    /// Address and port of the local node's JSON-RPC endpoint.
    ///
    /// The sweep is designed for a local, fully synced node: it pipelines
    /// requests at `--concurrency`, which is impolite to remote public nodes.
    #[structopt(long, default_value = "127.0.0.1:8232")]
    pub rpc_addr: SocketAddr,

    /// Network to assemble the known-hash list for: `mainnet` or `testnet`.
    #[structopt(long, default_value = "mainnet")]
    pub network: Network,

    /// First height to sweep.
    ///
    /// Sweeps are resumable: heights already present in both output files are
    /// always skipped, so this is only needed to restrict the swept range.
    #[structopt(long, default_value = "0")]
    pub start_height: Height,

    /// Last height to sweep or emit, inclusive.
    ///
    /// During sweeps this defaults to the node tip minus a confirmation margin
    /// of 100 blocks. It is required by `--emit-chunks`, where it becomes the
    /// reviewed `max_height` of the emitted `KnownHashListSpec`.
    #[structopt(long)]
    pub end_height: Option<Height>,

    /// Directory for the swept hash file and the emitted chunk assets.
    #[structopt(long)]
    pub out_dir: PathBuf,

    /// Per-height block size file: a `u32` little-endian value at offset
    /// `height * 4`, where `0` means "not fetched yet".
    ///
    /// This format is compatible with existing block size caches like
    /// `~/.cache/zebra-ibd/block-sizes-mainnet.u32le`. Defaults to
    /// `<out-dir>/block-sizes-<network>.u32le`.
    #[structopt(long)]
    pub sizes_file: Option<PathBuf>,

    /// Number of concurrent RPC requests during the sweep.
    #[structopt(long, default_value = "32")]
    pub concurrency: usize,

    /// Convert completed sweep files into the chunked hash assets and the
    /// size-hint array, and print the `KnownHashListSpec` constant block,
    /// instead of sweeping.
    ///
    /// Requires `--end-height`, and a sweep that covers every height up to it.
    #[structopt(long)]
    pub emit_chunks: bool,
}
