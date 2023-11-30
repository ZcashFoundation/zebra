//! Initializing the scanner.

use color_eyre::Report;
use tokio::task::JoinHandle;
use tracing::Instrument;

use zebra_chain::{diagnostic::task::WaitForPanics, parameters::Network};

use crate::{scan, storage::Storage, Config};

/// Initialize the scanner based on its config, and spawn a task for it.
///
/// TODO: add a test for this function.
pub fn spawn_init(
    config: &Config,
    network: Network,
    state: scan::State,
) -> JoinHandle<Result<(), Report>> {
    let config = config.clone();
    tokio::spawn(init(config, network, state).in_current_span())
}

/// Initialize the scanner based on its config.
///
/// TODO: add a test for this function.
pub async fn init(config: Config, network: Network, state: scan::State) -> Result<(), Report> {
    let storage = tokio::task::spawn_blocking(move || Storage::new(&config, network))
        .wait_for_panics()
        .await;

    // TODO: add more tasks here?
    scan::start(state, storage).await
}
