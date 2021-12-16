//! The timestamp collector collects liveness information from peers.

use std::{net::SocketAddr, sync::Arc};

use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

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
    /// - the address book,
    /// - the transmission channel for address book update events, and
    /// - the address book updater task.
    ///
    /// The task exits with an error when the returned [`mpsc::Sender`] is closed.
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

        let worker = move || {
            while let Some(event) = worker_rx.blocking_recv() {
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

        // Correctness: spawn address book accesses on a blocking thread,
        //              to avoid deadlocks (see #1976)
        let address_book_updater_task_handle = tokio::task::spawn_blocking(worker);

        (address_book, worker_tx, address_book_updater_task_handle)
    }
}
