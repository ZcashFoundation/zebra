//! Management of peer liveness / last-seen information.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures::channel::mpsc;
use tokio::prelude::*;
use tracing::Level;
use tracing_futures::Instrument;

/// Maintains a lookup table from peer addresses to last-seen times.
#[derive(Clone, Debug)]
pub struct AddressBook {
    data: Arc<Mutex<AddressData>>,
    shutdown: Arc<ShutdownSignal>,
    worker_tx: AddressBookSender,
}

#[derive(Default, Debug)]
struct AddressData {
    by_addr: HashMap<SocketAddr, DateTime<Utc>>,
    by_time: BTreeMap<DateTime<Utc>, SocketAddr>,
}

/// A handle for sending data to an AddressBook.
pub(crate) type AddressBookSender = mpsc::Sender<(SocketAddr, DateTime<Utc>)>;

impl AddressData {
    fn update(&mut self, addr: SocketAddr, timestamp: DateTime<Utc>) {
        use std::collections::hash_map::Entry;
        trace!(?addr, ?timestamp, "updating entry in addressdata");
        match self.by_addr.entry(addr) {
            Entry::Occupied(mut entry) => {
                let last_timestamp = entry.get();
                self.by_time
                    .remove(last_timestamp)
                    .expect("cannot have by_addr entry without by_time entry");
                entry.insert(timestamp);
                self.by_time.insert(timestamp, addr);
            }
            Entry::Vacant(entry) => {
                entry.insert(timestamp);
                self.by_time.insert(timestamp, addr);
            }
        }
    }
}

impl AddressBook {
    /// Constructs a new `AddressBook`, spawning a worker task to process updates.
    pub fn new() -> AddressBook {
        let data = Arc::new(Mutex::new(AddressData::default()));
        // We need to make a copy now so we can move data into the async block.
        let data2 = data.clone();

        // Construct and then spawn a worker.
        let worker_span = span!(Level::TRACE, "AddressBookWorker");
        const ADDRESS_WORKER_BUFFER_SIZE: usize = 100;
        let (worker_tx, mut worker_rx) = mpsc::channel(ADDRESS_WORKER_BUFFER_SIZE);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(0);
        let worker = async move {
            use futures::select;
            loop {
                select! {
                    _ = shutdown_rx.next() => return,
                    msg = worker_rx.next() => {
                        match msg {
                            Some((addr, timestamp)) => {
                                data2
                                    .lock()
                                    .expect("mutex should be unpoisoned")
                                    .update(addr, timestamp)
                            }
                            None => return,
                        }
                    }
                }
            }
        };
        tokio::spawn(worker.instrument(worker_span).boxed());

        AddressBook {
            data,
            worker_tx,
            shutdown: Arc::new(ShutdownSignal { tx: shutdown_tx }),
        }
    }

    pub(crate) fn sender_handle(&self) -> AddressBookSender {
        self.worker_tx.clone()
    }
}

/// Sends a signal when dropped.
#[derive(Debug)]
struct ShutdownSignal {
    /// Sending () signals that the task holding the rx end should terminate.
    ///
    /// This should really be a oneshot but calling a oneshot consumes it,
    /// and we can't move out of self in Drop.
    tx: mpsc::Sender<()>,
}

impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        self.tx.try_send(()).expect("tx is only used in drop");
    }
}
