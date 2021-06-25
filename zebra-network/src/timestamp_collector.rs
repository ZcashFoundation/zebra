//! The timestamp collector collects liveness information from peers.

use std::{net::SocketAddr, sync::Arc};

use futures::{channel::mpsc, prelude::*};

use crate::{meta_addr::MetaAddrChange, AddressBook};

/// The timestamp collector hooks into incoming message streams for each peer and
/// records per-connection last-seen timestamps into an [`AddressBook`].
pub struct TimestampCollector {}

impl TimestampCollector {
    /// Spawn a new [`TimestampCollector`] task, updating a new [`AddressBook`]
    /// configured with a `local_listener`.
    ///
    /// Returns handles for the transmission channel for timestamp events, and
    /// the address book.
    pub fn spawn(
        local_listener: SocketAddr,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        mpsc::Sender<MetaAddrChange>,
    ) {
        use tracing::Level;
        const TIMESTAMP_WORKER_BUFFER_SIZE: usize = 100;
        let (worker_tx, mut worker_rx) = mpsc::channel(TIMESTAMP_WORKER_BUFFER_SIZE);
        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new(
            local_listener,
            span!(Level::TRACE, "timestamp collector"),
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
        };
        tokio::spawn(worker.boxed());

        (address_book, worker_tx)
    }
}
