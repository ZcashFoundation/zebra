//! Management of peer liveness / last-seen information.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures::channel::mpsc;
use tokio::prelude::*;

/// A type alias for a timestamp event sent to a `TimestampCollector`.
pub(crate) type PeerTimestamp = (SocketAddr, DateTime<Utc>);

/// Maintains a lookup table from peer addresses to last-seen times.
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
    data: Arc<Mutex<TimestampData>>,
    shutdown: Arc<ShutdownSignal>,
    worker_tx: mpsc::Sender<PeerTimestamp>,
}

#[derive(Default, Debug)]
struct TimestampData {
    by_addr: HashMap<SocketAddr, DateTime<Utc>>,
    by_time: BTreeMap<DateTime<Utc>, SocketAddr>,
}

impl TimestampData {
    fn update(&mut self, event: PeerTimestamp) {
        use std::collections::hash_map::Entry;
        let (addr, timestamp) = event;
        trace!(?addr, ?timestamp);
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

impl TimestampCollector {
    /// Constructs a new `TimestampCollector`, spawning a worker task to process updates.
    pub fn new() -> TimestampCollector {
        let data = Arc::new(Mutex::new(TimestampData::default()));
        // We need to make a copy now so we can move data into the async block.
        let data2 = data.clone();

        const TIMESTAMP_WORKER_BUFFER_SIZE: usize = 100;
        let (worker_tx, mut worker_rx) = mpsc::channel(TIMESTAMP_WORKER_BUFFER_SIZE);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(0);

        // Construct and then spawn a worker.
        let worker = async move {
            use futures::select;
            loop {
                select! {
                    _ = shutdown_rx.next() => return,
                    msg = worker_rx.next() => {
                        match msg {
                            Some(event) => {
                                data2
                                    .lock()
                                    .expect("mutex should be unpoisoned")
                                    .update(event)
                            }
                            None => return,
                        }
                    }
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

    pub(crate) fn sender_handle(&self) -> mpsc::Sender<PeerTimestamp> {
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
