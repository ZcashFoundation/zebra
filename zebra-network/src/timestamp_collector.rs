//! The timestamp collector collects liveness information from peers.

use std::sync::Arc;

use futures::{channel::mpsc, prelude::*};

use crate::{
    address_book::spawn_blocking, constants, meta_addr::MetaAddrChange, AddressBook, Config,
};

/// The timestamp collector hooks into incoming message streams for each peer and
/// records per-connection [`MetaAddrChange`]s into an [`AddressBook`].
pub struct TimestampCollector {}

impl TimestampCollector {
    /// Spawn a new [`TimestampCollector`] task.
    ///
    /// Return handles for:
    /// * the transmission channel for [`MetaAddrChange`] events, and
    /// * the [`AddressBook`] it updates.
    pub fn spawn(
        config: Config,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        mpsc::Sender<MetaAddrChange>,
    ) {
        use tracing::Level;
        let (worker_tx, worker_rx) = mpsc::channel(constants::TIMESTAMP_WORKER_BUFFER_SIZE);
        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new(
            config,
            span!(Level::TRACE, "timestamp collector"),
        )));
        let worker_address_book = address_book.clone();

        let worker = async move {
            // # Performance
            //
            // The timestamp collector gets a change for every message, and locks
            // the address book for every update. So we merge all ready changes
            // into a single address book update.
            let mut worker_rx = worker_rx.ready_chunks(constants::TIMESTAMP_WORKER_BUFFER_SIZE);
            while let Some(chunk) = worker_rx.next().await {
                // # Correctness
                //
                // Briefly hold the address book threaded mutex on a blocking thread.
                spawn_blocking(&worker_address_book.clone(), move |address_book| {
                    address_book.extend(chunk)
                })
                .await;
            }
        };
        tokio::spawn(worker.boxed());

        (address_book, worker_tx)
    }
}
