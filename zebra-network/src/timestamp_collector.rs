//! The timestamp collector collects liveness information from peers.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures::channel::mpsc;
use tokio::prelude::*;

use crate::{
    constants,
    types::{MetaAddr, PeerServices},
    AddressBook,
};

/// The timestamp collector hooks into incoming message streams for each peer and
/// records per-connection last-seen timestamps into an [`AddressBook`].
///
/// On creation, the `TimestampCollector` spawns a worker task to process new
/// timestamp events. The resulting `TimestampCollector` can be cloned, and the
/// worker task and state are shared among all of the clones.
///
/// XXX add functionality for querying the timestamp data
#[derive(Clone, Debug)]
pub struct TimestampCollector {
    // We do not expect mutex contention to be a problem, because
    // the dominant accessor is the collector worker, and it has a long
    // event buffer to hide latency if other tasks block it temporarily.
    data: Arc<Mutex<AddressBook>>,
    shutdown: Arc<ShutdownSignal>,
    worker_tx: mpsc::Sender<MetaAddr>,
}

impl TimestampCollector {
    /// Constructs a new `TimestampCollector`, spawning a worker task to process updates.
    pub fn new() -> TimestampCollector {
        let data = Arc::new(Mutex::new(AddressBook::default()));
        // We need to make a copy now so we can move data into the async block.
        let data2 = data.clone();

        const TIMESTAMP_WORKER_BUFFER_SIZE: usize = 100;
        let (worker_tx, mut worker_rx) = mpsc::channel(TIMESTAMP_WORKER_BUFFER_SIZE);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(0);

        // Construct and then spawn a worker.
        let worker = async move {
            use futures::future::{self, Either};
            loop {
                match future::select(shutdown_rx.next(), worker_rx.next()).await {
                    Either::Left((_, _)) => return,     // shutdown signal
                    Either::Right((None, _)) => return, // all workers are gone
                    Either::Right((Some(event), _)) => data2
                        .lock()
                        .expect("mutex should be unpoisoned")
                        .update(event),
                }
            }
        };
        tokio::spawn(worker.boxed());

        TimestampCollector {
            data,
            worker_tx,
            shutdown: Arc::new(ShutdownSignal { tx: shutdown_tx }),
        }
    }

    /// Return a shared reference to the [`AddressBook`] this collector updates.
    pub fn address_book(&self) -> Arc<Mutex<AddressBook>> {
        self.data.clone()
    }

    pub(crate) fn sender_handle(&self) -> mpsc::Sender<MetaAddr> {
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
