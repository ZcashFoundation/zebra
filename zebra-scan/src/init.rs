//! Initializing the scanner.

use std::sync::{
    mpsc::{self},
    Arc,
};

use color_eyre::Report;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::ServiceBuilder;

use zebra_chain::{parameters::Network, transaction::Transaction};
use zebra_state::ChainTipChange;

use crate::{scan, service::ScanService, Config};

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
        keys: Vec<String>,
    },

    /// Start sending results for key hashes to `result_sender`
    SubscribeResults {
        /// Sender for results
        result_sender: mpsc::Sender<Arc<Transaction>>,

        /// Key hashes to send the results of to result channel
        key_hashes: Vec<()>,
    },
}

#[derive(Debug, Clone)]
/// Scan task handle and command channel sender
pub struct ScanTask {
    /// [`JoinHandle`] of scan task
    pub handle: Arc<JoinHandle<Result<(), Report>>>,

    /// Task command channel sender
    pub cmd_sender: mpsc::Sender<ScanTaskCommand>,
}

impl ScanTask {
    /// Spawns a new [`ScanTask`] for tests.
    pub fn mock() -> Self {
        let (cmd_sender, _cmd_receiver) = mpsc::channel();

        Self {
            handle: Arc::new(tokio::spawn(std::future::pending())),
            cmd_sender,
        }
    }

    /// Spawns a new [`ScanTask`].
    pub fn spawn(
        config: &Config,
        network: Network,
        state: scan::State,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        // TODO: Pass `_cmd_receiver` to `scan::start()` to pass it new keys after it's been spawned
        let (cmd_sender, cmd_receiver) = mpsc::channel();

        Self {
            handle: Arc::new(scan::spawn_init(
                config,
                network,
                state,
                chain_tip_change,
                cmd_receiver,
            )),
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

    /// Sends a command to the scan task
    pub fn remove_keys(
        &mut self,
        keys: &[String],
    ) -> Result<oneshot::Receiver<()>, mpsc::SendError<ScanTaskCommand>> {
        let (done_tx, done_rx) = oneshot::channel();

        self.send(ScanTaskCommand::RemoveKeys {
            keys: keys.to_vec(),
            done_tx,
        })?;

        Ok(done_rx)
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
) -> ScanTask {
    ScanTask::spawn(config, network, state, chain_tip_change)
}

/// Initialize [`ScanService`] based on its config.
///
/// TODO: add a test for this function.
pub async fn init(
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> Result<(), Report> {
    let scan_service = ServiceBuilder::new().buffer(10).service(ScanService::new(
        &config,
        network,
        state,
        chain_tip_change,
    ));

    // Start the gRPC server.
    zebra_grpc::server::init(scan_service).await?;

    Ok(())
}
