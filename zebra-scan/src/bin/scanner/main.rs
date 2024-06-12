//! The zebra-scanner binary.
//!
//! The zebra-scanner binary is a standalone binary that scans the Zcash blockchain for transactions using the given sapling keys.
use lazy_static::lazy_static;
use structopt::StructOpt;
use tracing::*;

use zebra_chain::parameters::Network;
use zebra_state::{ChainTipSender, SaplingScanningKey};

use core::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
/// Runs the zebra scanner binary with the given arguments.
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Display all logs from the zebra-scan crate.
    tracing_subscriber::fmt::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Parse command line arguments.
    let args = Args::from_args();
    let network = args.network;
    let sapling_keys_to_scan = args
        .sapling_keys_to_scan
        .into_iter()
        .map(|key| (key, 1))
        .collect();
    let cache_dir = args.cache_dir;
    let listen_addr = args.listen_addr;

    // Create a state config with arguments.
    let state_config = zebra_state::Config {
        cache_dir,
        ..zebra_state::Config::default()
    };

    // Create a scanner config with arguments.
    let scanner_config = zebra_scan::Config {
        sapling_keys_to_scan,
        listen_addr,
        ..zebra_scan::Config::default()
    };

    // Get a read-only state and the database.
    let (read_state, db, _) = zebra_state::init_read_only(state_config, &network);

    // Get the initial tip block from the database.
    let initial_tip = db
        .tip_block()
        .map(zebra_state::CheckpointVerifiedBlock::from)
        .map(zebra_state::ChainTipBlock::from);

    // Create a chain tip sender and use it to get a chain tip change.
    let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(initial_tip, &network);

    // Spawn the scan task.
    let scan_task_handle =
        { zebra_scan::spawn_init(scanner_config, network, read_state, chain_tip_change) };

    // Pin the scan task handle.
    tokio::pin!(scan_task_handle);

    // Wait for task to finish
    loop {
        let _result = tokio::select! {
            scan_result = &mut scan_task_handle => scan_result
                .expect("unexpected panic in the scan task")
                .map(|_| info!("scan task exited")),
        };
    }
}

// Default values for the zebra-scanner arguments.
lazy_static! {
    static ref DEFAULT_CACHE_DIR: String = zebra_state::Config::default()
        .cache_dir
        .to_str()
        .expect("default cache dir is valid")
        .to_string();
    static ref DEFAULT_NETWORK: String = Network::default().to_string();
}

/// zebra-scanner arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// Path to an existing zebra state cache directory.
    #[structopt(default_value = &DEFAULT_CACHE_DIR, short, long)]
    pub cache_dir: PathBuf,

    /// The Zcash network where the scanner will run.
    #[structopt(default_value = &DEFAULT_NETWORK, short, long)]
    pub network: Network,

    /// The sapling keys to scan for.
    #[structopt(short, long)]
    pub sapling_keys_to_scan: Vec<SaplingScanningKey>,

    /// IP address and port for the zebra-scan gRPC server.
    #[structopt(short, long)]
    pub listen_addr: Option<SocketAddr>,
}
