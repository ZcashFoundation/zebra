//! The timestamp collector collects liveness information from peers.

use crate::{types::MetaAddr, AddressBook};
use futures::{channel::mpsc, prelude::*};
use std::sync::{Arc, Mutex};

/// The timestamp collector hooks into incoming message streams for each peer and
/// records per-connection last-seen timestamps into an [`AddressBook`].
pub struct TimestampCollector {}

impl TimestampCollector {
    /// Spawn a new [`TimestampCollector`] task, and return handles for the
    /// transmission channel for timestamp events and for the [`AddressBook`] it
    /// updates.
    pub fn spawn() -> (Arc<Mutex<AddressBook>>, mpsc::Sender<MetaAddr>) {
        use tracing::Level;
        const TIMESTAMP_WORKER_BUFFER_SIZE: usize = 100;
        let (worker_tx, mut worker_rx) = mpsc::channel(TIMESTAMP_WORKER_BUFFER_SIZE);
        let address_book = Arc::new(Mutex::new(AddressBook::new(span!(
            Level::TRACE,
            "timestamp collector"
        ))));
        let worker_address_book = address_book.clone();

        let worker = async move {
            while let Some(event) = worker_rx.next().await {
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
