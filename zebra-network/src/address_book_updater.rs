//! The timestamp collector collects liveness information from peers.

use std::{net::SocketAddr, sync::Arc};

use futures::{channel::mpsc, prelude::*};
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::{meta_addr::MetaAddrChange, AddressBook, BoxError, Config};

/// The `AddressBookUpdater` hooks into incoming message streams for each peer
/// and lets the owner of the sender handle update the address book. For
/// example, it can be used to record per-connection last-seen timestamps, or
/// add new initial peers to the address book.
#[derive(Debug, Eq, PartialEq)]
pub struct AddressBookUpdater;

#[derive(Copy, Clone, Debug, Error, Eq, PartialEq, Hash)]
#[error("all address book updater senders are closed")]
pub struct AllAddressBookUpdaterSendersClosed;

impl AddressBookUpdater {
    /// Spawn a new [`AddressBookUpdater`] task, updating a new [`AddressBook`]
    /// configured with Zebra's actual `local_listener` address.
    ///
    /// Returns handles for:
    /// - the transmission channel for address book update events,
    /// - the address book, and
    /// - the address book updater task.
    pub fn spawn(
        config: &Config,
        local_listener: SocketAddr,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        mpsc::Sender<MetaAddrChange>,
        JoinHandle<Result<(), BoxError>>,
    ) {
        use tracing::Level;

        // Create an mpsc channel for peerset address book updates,
        // based on the maximum number of inbound and outbound peers.
        let (worker_tx, mut worker_rx) = mpsc::channel(config.peerset_total_connection_limit());

        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new(
            local_listener,
            span!(Level::TRACE, "address book updater"),
        )));
        let worker_address_book = address_book.clone();

        let worker = async move {
            while let Some(event) = worker_rx.next().await {
                // # Correctness
                //
                // Briefly hold the address book threaded mutex, to update the
                // state for a single address.
                worker_address_book
                    .lock()
                    .expect("mutex should be unpoisoned")
                    .update(event);
            }

            Err(AllAddressBookUpdaterSendersClosed.into())
        };

        let address_book_updater_task_handle = tokio::spawn(worker.boxed());

        (address_book, worker_tx, address_book_updater_task_handle)
    }
}
