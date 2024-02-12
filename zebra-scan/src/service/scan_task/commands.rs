//! Types and method implementations for [`ScanTaskCommand`]

use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Receiver},
};

use color_eyre::{eyre::eyre, Report};
use tokio::sync::{mpsc::error::TrySendError, oneshot};

use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};
use zebra_chain::{block::Height, parameters::Network, transaction};
use zebra_state::SaplingScanningKey;

use crate::scan::sapling_key_to_scan_block_keys;

use super::ScanTask;

#[derive(Debug)]
/// Commands that can be sent to [`ScanTask`]
pub enum ScanTaskCommand {
    /// Start scanning for new viewing keys
    RegisterKeys {
        /// New keys to start scanning for
        keys: Vec<(String, Option<u32>)>,
        /// Returns the set of keys the scanner accepted.
        rsp_tx: oneshot::Sender<Vec<SaplingScanningKey>>,
    },

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
        result_sender: mpsc::Sender<transaction::Hash>,

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
        cmd_receiver: &mut tokio::sync::mpsc::Receiver<ScanTaskCommand>,
        registered_keys: &mut HashMap<
            SaplingScanningKey,
            (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>),
        >,
        network: Network,
    ) -> Result<
        (
            HashMap<
                SaplingScanningKey,
                (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height),
            >,
            HashMap<SaplingScanningKey, mpsc::Sender<transaction::Hash>>,
        ),
        Report,
    > {
        use tokio::sync::mpsc::error::TryRecvError;

        let mut new_keys = HashMap::new();
        let mut new_result_senders = HashMap::new();
        let sapling_activation_height = network.sapling_activation_height();

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
                ScanTaskCommand::RegisterKeys { keys, rsp_tx } => {
                    // Determine what keys we pass to the scanner.
                    let keys: Vec<_> = keys
                        .into_iter()
                        .filter_map(|key| {
                            // Don't accept keys that:
                            // 1. the scanner already has, and
                            // 2. were already submitted.
                            if registered_keys.contains_key(&key.0)
                                && !new_keys.contains_key(&key.0)
                            {
                                return None;
                            }

                            let birth_height = if let Some(height) = key.1 {
                                match Height::try_from(height) {
                                    Ok(height) => height,
                                    // Don't accept the key if its birth height is not a valid height.
                                    Err(_) => return None,
                                }
                            } else {
                                // Use the Sapling activation height if the key has no birth height.
                                sapling_activation_height
                            };

                            sapling_key_to_scan_block_keys(&key.0, network)
                                .ok()
                                .map(|parsed| (key.0, (parsed.0, parsed.1, birth_height)))
                        })
                        .collect();

                    // Send the accepted keys back.
                    let _ = rsp_tx.send(keys.iter().map(|key| key.0.clone()).collect());

                    new_keys.extend(keys.clone());

                    registered_keys.extend(
                        keys.into_iter()
                            .map(|(key, (dfvks, ivks, _))| (key, (dfvks, ivks))),
                    );
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
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<ScanTaskCommand>> {
        self.cmd_sender.try_send(command)
    }

    /// Sends a message to the scan task to remove the provided viewing keys.
    ///
    /// Returns a oneshot channel receiver to notify the caller when the keys have been removed.
    pub fn remove_keys(
        &mut self,
        keys: &[String],
    ) -> Result<oneshot::Receiver<()>, TrySendError<ScanTaskCommand>> {
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
        keys: Vec<(String, Option<u32>)>,
    ) -> Result<oneshot::Receiver<Vec<String>>, TrySendError<ScanTaskCommand>> {
        let (rsp_tx, rsp_rx) = oneshot::channel();

        self.send(ScanTaskCommand::RegisterKeys { keys, rsp_tx })?;

        Ok(rsp_rx)
    }

    /// Sends a message to the scan task to start sending the results for the provided viewing keys to a channel.
    ///
    /// Returns the channel receiver.
    pub fn subscribe(
        &mut self,
        keys: HashSet<SaplingScanningKey>,
    ) -> Result<Receiver<transaction::Hash>, TrySendError<ScanTaskCommand>> {
        // TODO: Use a bounded channel
        let (result_sender, result_receiver) = mpsc::channel();

        self.send(ScanTaskCommand::SubscribeResults {
            result_sender,
            keys,
        })
        .map(|_| result_receiver)
    }
}
