//! Initializing the scanner.

use color_eyre::Report;
use tokio::task::JoinHandle;
use tracing::Instrument;

use zebra_chain::{diagnostic::task::WaitForPanics, parameters::Network};
use zebra_state::ChainTipChange;

use crate::{scan, storage::Storage, Config};

/// Initialize the scanner based on its config, and spawn a task for it.
///
/// TODO: add a test for this function.
pub fn spawn_init(
    config: &Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> JoinHandle<Result<(), Report>> {
    let config = config.clone();

    // TODO: spawn an entirely new executor here, to avoid timing attacks.
    tokio::spawn(init(config, network, state, chain_tip_change).in_current_span())
}

/// Initialize the scanner based on its config.
///
/// TODO: add a test for this function.
pub async fn init(
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> Result<(), Report> {
    let storage = tokio::task::spawn_blocking(move || Storage::new(&config, network))
        .wait_for_panics()
        .await;

    // TODO: add more tasks here?
    scan::start(state, chain_tip_change, storage).await
}
