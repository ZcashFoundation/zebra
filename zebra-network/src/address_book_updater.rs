//! The timestamp collector collects liveness information from peers.

use std::{
    cmp::max,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use indexmap::IndexMap;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{Level, Span};

use crate::{
    address_book::AddressMetrics, meta_addr::MetaAddrChange, AddressBook, BoxError, Config,
};

/// The minimum size of the address book updater channel.
pub const MIN_CHANNEL_SIZE: usize = 10;

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
    /// - the transmission channel for address book update events,
    /// - a watch channel for address book metrics, and
    /// - the address book updater task join handle.
    ///
    /// The task exits with an error when the returned [`mpsc::Sender`] is closed.
    pub fn spawn(
        config: &Config,
        local_listener: SocketAddr,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        watch::Receiver<Arc<IndexMap<IpAddr, Instant>>>,
        mpsc::Sender<MetaAddrChange>,
        watch::Receiver<AddressMetrics>,
        JoinHandle<Result<(), BoxError>>,
    ) {
        // Create an mpsc channel for peerset address book updates,
        // based on the maximum number of inbound and outbound peers.
        let (worker_tx, mut worker_rx) = mpsc::channel(max(
            config.peerset_total_connection_limit(),
            MIN_CHANNEL_SIZE,
        ));

        let address_book = AddressBook::new(
            local_listener,
            &config.network,
            config.max_connections_per_ip,
            span!(Level::TRACE, "address book"),
        );
        let address_metrics = address_book.address_metrics_watcher();
        let address_book = Arc::new(std::sync::Mutex::new(address_book));

        #[cfg(feature = "progress-bar")]
        let (mut address_info, address_bar, never_bar, failed_bar) = {
            let address_bar = howudoin::new_root().label("Known Peers");
            let never_bar =
                howudoin::new_with_parent(address_bar.id()).label("Never Attempted Peers");
            let failed_bar = howudoin::new_with_parent(never_bar.id()).label("Failed Peers");

            (address_metrics.clone(), address_bar, never_bar, failed_bar)
        };

        let worker_address_book = address_book.clone();
        let (bans_sender, bans_receiver) = tokio::sync::watch::channel(
            worker_address_book
                .lock()
                .expect("mutex should be unpoisoned")
                .bans(),
        );

        let worker = move || {
            info!("starting the address book updater");

            while let Some(event) = worker_rx.blocking_recv() {
                trace!(?event, "got address book change");

                // # Correctness
                //
                // Briefly hold the address book threaded mutex, to update the
                // state for a single address.
                let updated = worker_address_book
                    .lock()
                    .expect("mutex should be unpoisoned")
                    .update(event);

                // `UpdateMisbehavior` events should only be passed to `update()` here,
                // so that this channel is always updated when new addresses are banned.
                if updated.is_none() {
                    let bans = worker_address_book
                        .lock()
                        .expect("mutex should be unpoisoned")
                        .bans();

                    if bans.contains_key(&event.addr().ip()) {
                        let _ = bans_sender.send(bans);
                    }
                }

                #[cfg(feature = "progress-bar")]
                if matches!(howudoin::cancelled(), Some(true)) {
                    address_bar.close();
                    never_bar.close();
                    failed_bar.close();
                } else if address_info.has_changed()? {
                    // We don't track:
                    // - attempt pending because it's always small
                    // - responded because it's the remaining attempted-but-not-failed peers
                    // - recently live because it's similar to the connected peer counts

                    let address_info = *address_info.borrow_and_update();

                    address_bar
                        .set_pos(u64::try_from(address_info.num_addresses).expect("fits in u64"));
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));

                    never_bar.set_pos(
                        u64::try_from(address_info.never_attempted_gossiped).expect("fits in u64"),
                    );
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));

                    failed_bar.set_pos(u64::try_from(address_info.failed).expect("fits in u64"));
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));
                }
            }

            #[cfg(feature = "progress-bar")]
            {
                address_bar.close();
                never_bar.close();
                failed_bar.close();
            }

            let error = Err(AllAddressBookUpdaterSendersClosed.into());
            info!(?error, "stopping address book updater");
            error
        };

        // Correctness: spawn address book accesses on a blocking thread,
        //              to avoid deadlocks (see #1976)
        let span = Span::current();
        let address_book_updater_task_handle =
            tokio::task::spawn_blocking(move || span.in_scope(worker));

        (
            address_book,
            bans_receiver,
            worker_tx,
            address_metrics,
            address_book_updater_task_handle,
        )
    }
}
