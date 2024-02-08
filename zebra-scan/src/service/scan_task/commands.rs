//! Types and method implementations for [`ScanTaskCommand`]

use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use color_eyre::{eyre::eyre, Report};
use tokio::sync::oneshot;

use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};
use zebra_chain::block::Height;
use zebra_state::{SaplingScannedResult, SaplingScanningKey};

use super::ScanTask;

#[derive(Debug)]
/// Commands that can be sent to [`ScanTask`]
pub enum ScanTaskCommand {
    /// Start scanning for new viewing keys
    RegisterKeys {
        /// New keys to start scanning for
        keys: HashMap<
            SaplingScanningKey,
            (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height),
        >,
    },

    /// Stop scanning for deleted viewing keys
    RemoveKeys {
        /// Notify the caller once the key is removed (so the caller can wait before clearing results)
        done_tx: oneshot::Sender<()>,

        /// Key hashes that are to be removed
        keys: Vec<String>,
    },

    /// Start sending results for key hashes to `result_sender`
    // TODO: Implement this command (#8206)
    SubscribeResults {
        /// Sender for results
        // TODO: Update type to return full `WalletTx`
        result_sender: mpsc::Sender<SaplingScannedResult>,

        /// Key hashes to send the results of to result channel
        keys: HashSet<String>,
    },
}

impl ScanTask {
    /// Accepts the scan task's `parsed_key` collection and a reference to the command channel receiver
    ///
    /// Processes messages in the scan task channel, updating `parsed_keys` if required.
    ///
    /// Returns newly registered keys for scanning.
    pub fn process_messages(
        cmd_receiver: &Receiver<ScanTaskCommand>,
        registered_keys: &mut HashMap<
            SaplingScanningKey,
            (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>),
        >,
    ) -> Result<
        (
            HashMap<
                SaplingScanningKey,
                (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height),
            >,
            HashMap<SaplingScanningKey, mpsc::Sender<SaplingScannedResult>>,
        ),
        Report,
    > {
        let mut new_keys = HashMap::new();
        let mut new_result_senders = HashMap::new();

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
                ScanTaskCommand::RegisterKeys { keys } => {
                    let keys: Vec<_> = keys
                        .into_iter()
                        .filter(|(key, _)| {
                            !registered_keys.contains_key(key) || new_keys.contains_key(key)
                        })
                        .collect();

                    if !keys.is_empty() {
                        new_keys.extend(keys.clone());

                        let keys =
                            keys.into_iter()
                                .map(|(key, (decoded_dfvks, decoded_ivks, _h))| {
                                    (key, (decoded_dfvks, decoded_ivks))
                                });

                        registered_keys.extend(keys);
                    }
                }

                ScanTaskCommand::RemoveKeys { done_tx, keys } => {
                    for key in keys {
                        registered_keys.remove(&key);
                        new_keys.remove(&key);
                    }

                    // Ignore send errors for the done notification, caller is expected to use a timeout.
                    let _ = done_tx.send(());
                }

                ScanTaskCommand::SubscribeResults {
                    result_sender,
                    keys,
                } => {
                    let keys = keys
                        .into_iter()
                        .filter(|key| registered_keys.contains_key(key));

                    for key in keys {
                        new_result_senders.insert(key, result_sender.clone());
                    }
                }
            }
        }

        Ok((new_keys, new_result_senders))
    }

    /// Sends a command to the scan task
    pub fn send(
        &mut self,
        command: ScanTaskCommand,
    ) -> Result<(), mpsc::SendError<ScanTaskCommand>> {
        self.cmd_sender.send(command)
    }

    /// Sends a message to the scan task to remove the provided viewing keys.
    ///
    /// Returns a oneshot channel receiver to notify the caller when the keys have been removed.
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

    /// Sends a message to the scan task to start scanning for the provided viewing keys.
    pub fn register_keys(
        &mut self,
        keys: HashMap<
            SaplingScanningKey,
            (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height),
        >,
    ) -> Result<(), mpsc::SendError<ScanTaskCommand>> {
        self.send(ScanTaskCommand::RegisterKeys { keys })
    }

    /// Sends a message to the scan task to start sending the results for the provided viewing keys to a channel.
    ///
    /// Returns the channel receiver.
    pub fn subscribe(
        &mut self,
        keys: HashSet<SaplingScanningKey>,
    ) -> Result<Receiver<SaplingScannedResult>, mpsc::SendError<ScanTaskCommand>> {
        // TODO: Use a bounded channel
        let (result_sender, result_receiver) = mpsc::channel();

        self.send(ScanTaskCommand::SubscribeResults {
            result_sender,
            keys,
        })
        .map(|_| result_receiver)
    }
}
