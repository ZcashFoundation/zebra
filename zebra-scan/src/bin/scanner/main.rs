//! The zebra-scanner binary.
//!
//! The zebra-scanner binary is a standalone binary that scans the Zcash blockchain for transactions using the given sapling keys.
use color_eyre::eyre::eyre;
use lazy_static::lazy_static;
use structopt::StructOpt;
use tracing::*;

use zebra_chain::{block::Height, parameters::Network};
use zebra_state::SaplingScanningKey;

use core::net::SocketAddr;
use std::path::{Path, PathBuf};

/// A structure with sapling key and birthday height.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub struct SaplingKey {
    key: SaplingScanningKey,
    #[serde(default = "min_height")]
    birthday_height: Height,
}

fn min_height() -> Height {
    Height(0)
}

impl std::str::FromStr for SaplingKey {
    type Err = Box<dyn std::error::Error>;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(serde_json::from_str(value)?)
    }
}

#[tokio::main]
/// Runs the zebra scanner binary with the given arguments.
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Display logs with `info` level by default.
    let tracing_filter: String = match std::env::var("RUST_LOG") {
        Ok(val) if !val.is_empty() => val,
        _ => "info".to_string(),
    };

    tracing_subscriber::fmt::fmt()
        .with_env_filter(tracing_filter)
        .init();

    // Parse command line arguments.
    let args = Args::from_args();

    let zebrad_cache_dir = args.zebrad_cache_dir;
    validate_dir(&zebrad_cache_dir)?;

    let scanning_cache_dir = args.scanning_cache_dir;
    let mut db_config = zebra_scan::Config::default().db_config;
    db_config.cache_dir = scanning_cache_dir;
    let network = args.network;
    let sapling_keys_to_scan = args
        .sapling_keys_to_scan
        .into_iter()
        .map(|key| (key.key, key.birthday_height.0))
        .collect();
    let listen_addr = args.listen_addr;

    // Create a state config with arguments.
    let state_config = zebra_state::Config {
        cache_dir: zebrad_cache_dir,
        ..zebra_state::Config::default()
    };

    // Create a scanner config with arguments.
    let scanner_config = zebra_scan::Config {
        sapling_keys_to_scan,
        listen_addr,
        db_config,
    };

    // Get a read-only state and the database.
    let (read_state, _latest_chain_tip, chain_tip_change, sync_task) =
        zebra_rpc::sync::init_read_state_with_syncer(
            state_config,
            &network,
            args.zebra_rpc_listen_addr,
            args.zebra_indexer_rpc_listen_addr,
        )
        .await?
        .map_err(|err| eyre!(err))?;

    // Spawn the scan task.
    let scan_task_handle =
        { zebra_scan::spawn_init(scanner_config, network, read_state, chain_tip_change) };

    // Pin the scan task handle.
    tokio::pin!(scan_task_handle);
    tokio::pin!(sync_task);

    // Wait for task to finish
    tokio::select! {
        scan_result = &mut scan_task_handle => scan_result
            .expect("unexpected panic in the scan task")
            .map(|_| info!("scan task exited"))
            .map_err(Into::into),
        sync_result = &mut sync_task => {
            sync_result.expect("unexpected panic in the scan task");
            Ok(())
        }
    }
}

// Default values for the zebra-scanner arguments.
lazy_static! {
    static ref DEFAULT_ZEBRAD_CACHE_DIR: String = zebra_state::Config::default()
        .cache_dir
        .to_str()
        .expect("default cache dir is valid")
        .to_string();
    static ref DEFAULT_SCANNER_CACHE_DIR: String = zebra_scan::Config::default()
        .db_config
        .cache_dir
        .to_str()
        .expect("default cache dir is valid")
        .to_string();
    static ref DEFAULT_NETWORK: String = Network::default().to_string();
}

/// zebra-scanner arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// Path to zebrad state.
    #[structopt(default_value = &DEFAULT_ZEBRAD_CACHE_DIR, long)]
    pub zebrad_cache_dir: PathBuf,

    /// Path to scanning state.
    #[structopt(default_value = &DEFAULT_SCANNER_CACHE_DIR, long)]
    pub scanning_cache_dir: PathBuf,

    /// The Zcash network.
    #[structopt(default_value = &DEFAULT_NETWORK, long)]
    pub network: Network,

    /// The sapling keys to scan for.
    #[structopt(long)]
    pub sapling_keys_to_scan: Vec<SaplingKey>,

    /// The listen address of Zebra's RPC server used by the syncer to check for
    /// chain tip changes and get blocks in Zebra's non-finalized state.
    #[structopt(long)]
    pub zebra_rpc_listen_addr: SocketAddr,

    /// The listen address of Zebra's indexer gRPC server used by the syncer to
    /// listen for chain tip or mempool changes.
    #[structopt(long)]
    pub zebra_indexer_rpc_listen_addr: SocketAddr,

    /// IP address and port for the gRPC server.
    #[structopt(long)]
    pub listen_addr: Option<SocketAddr>,
}

/// Create an error message is a given directory does not exist or we don't have access to it for whatever reason.
fn validate_dir(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match dir.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => {
            let err_msg = format!("directory {} does not exist.", dir.display());
            error!("{}", err_msg);
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, err_msg).into())
        }
        Err(e) => {
            error!("directory {} could not be accessed: {:?}", dir.display(), e);
            Err(e.into())
        }
    }
}
