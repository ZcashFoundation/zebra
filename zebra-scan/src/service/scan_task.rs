//! Types and method implementations for [`ScanTask`]

use std::{
    collections::HashMap,
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
};

use color_eyre::{eyre::eyre, Report};
use tokio::{sync::oneshot, task::JoinHandle};
use tower::ServiceBuilder;

use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};
use zebra_chain::{parameters::Network, transaction::Transaction};
use zebra_state::{ChainTipChange, SaplingScanningKey};

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
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn mock() -> (Self, mpsc::Receiver<ScanTaskCommand>) {
        let (cmd_sender, cmd_receiver) = mpsc::channel();

        (
            Self {
                handle: Arc::new(tokio::spawn(std::future::pending())),
                cmd_sender,
            },
            cmd_receiver,
        )
    }

    /// Spawns a new [`ScanTask`].
    pub fn spawn(
        config: &Config,
        network: Network,
        state: scan::State,
        chain_tip_change: ChainTipChange,
    ) -> Self {
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

    /// Accepts the scan task's `parsed_key` collection and a reference to the command channel receiver
    ///
    /// Processes messages in the scan task channel, updating `parsed_keys` if required.
    ///
    /// Returns the updated `parsed_keys`
    pub fn process_messages(
        cmd_receiver: &Receiver<ScanTaskCommand>,
        mut parsed_keys: Arc<
            HashMap<SaplingScanningKey, (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>)>,
        >,
    ) -> Result<
        Arc<HashMap<SaplingScanningKey, (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>)>>,
        Report,
    > {
        loop {
            let cmd = match cmd_receiver.try_recv() {
                Ok(cmd) => cmd,

                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Return early if the sender has been dropped.
                    return Err(eyre!("command channel disconnected"));
                }
            };

            match cmd {
                ScanTaskCommand::RemoveKeys { done_tx, keys } => {
                    // TODO: Replace with Arc::unwrap_or_clone() when it stabilises:
                    // https://github.com/rust-lang/rust/issues/93610
                    let mut updated_parsed_keys =
                        Arc::try_unwrap(parsed_keys).unwrap_or_else(|arc| (*arc).clone());

                    for key in keys {
                        updated_parsed_keys.remove(&key);
                    }

                    parsed_keys = Arc::new(updated_parsed_keys);

                    // Ignore send errors for the done notification
                    let _ = done_tx.send(());
                }

                _ => continue,
            }
        }

        Ok(parsed_keys)
    }

    /// Sends a command to the scan task
    pub fn send(
        &mut self,
        command: ScanTaskCommand,
    ) -> Result<(), mpsc::SendError<ScanTaskCommand>> {
        self.cmd_sender.send(command)
    }

    /// Sends a message to the scan task to remove the provided viewing keys.
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
