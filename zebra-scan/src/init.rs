//! Initializing the scanner.

use color_eyre::Report;
use tokio::task::JoinHandle;
use tracing::Instrument;

use zebra_chain::parameters::Network;

use crate::{scan, storage::Storage, Config};

/// Initialize the scanner based on its config.
pub fn init(
    config: &Config,
    network: Network,
    state: scan::State,
) -> JoinHandle<Result<(), Report>> {
    let storage = Storage::new(config, network);

    // TODO: add more tasks here?
    tokio::spawn(scan::start(state, storage).in_current_span())
}
