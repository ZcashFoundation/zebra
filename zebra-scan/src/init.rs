//! Initializing the scanner.

use std::sync::{mpsc, Arc};

use color_eyre::Report;
use tokio::{sync::oneshot, task::JoinHandle};
use tracing::Instrument;

use zebra_chain::{chain_tip, diagnostic::task::WaitForPanics, parameters::Network, transaction::Transaction};
use zebra_node_services::scan_service;
use zebra_state::ChainTipChange;

use crate::{scan, service::ScanService, storage::Storage, Config};

#[derive(Debug)]
/// Commands that can be sent to [`ScanTask`]
pub enum ScanTaskCommand {
    /// Start scanning for new viewing keys
    RegisterKeys(Vec<()>), // TODO: send `ViewingKeyWithHash`es

    /// Stop scanning for deleted viewing keys
    RemoveKeys {
        /// Notify the caller once the key is removed (so the caller can wait before clearing results)
        done_tx: oneshot::Sender<()>,

        /// Key hashes that are to be removed
        key_hashes: Vec<()>,
    },

    /// Start sending results for key hashes to `result_sender`
    SubscribeResults {
        /// Sender for results
        result_sender: mpsc::Sender<Arc<Transaction>>,

        /// Key hashes to send the results of to result channel
        key_hashes: Vec<()>,
    },
}

#[derive(Debug)]
/// Scan task handle and command channel sender
pub struct ScanTask {
    /// [`JoinHandle`] of scan task
    pub handle: JoinHandle<Result<(), Report>>,

    /// Task command channel sender
    cmd_sender: mpsc::Sender<ScanTaskCommand>,
}

impl ScanTask {
    /// Spawns a new [`ScanTask`].
    pub fn spawn(
        config: &Config,
        network: Network,
        state: scan::State,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        // TODO: Pass `_cmd_receiver` to `scan::start()` to pass it new keys after it's been spawned
        let (cmd_sender, _cmd_receiver) = mpsc::channel();

        Self {
            handle: spawn_init(config, network, state, chain_tip_change),
            cmd_sender,
        }
    }

    /// Sends a command to the scan task
    pub fn send(
        &mut self,
        command: ScanTaskCommand,
    ) -> Result<(), mpsc::SendError<ScanTaskCommand>> {
        self.cmd_sender.send(command)
    }
}

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
    let scan_service: ScanService = ScanService::new(&config, network, state.clone(), chain_tip_chang.clone());

    let storage = tokio::task::spawn_blocking(move || Storage::new(&config, network, false))
        .wait_for_panics()
        .await;

    // Start the gRPC server.
    zebra_grpc::server::init(scan_service).await?;

    // TODO: add more tasks here?
    scan::start(state, chain_tip_change, storage).await
}
