//! The address book updater: spawns the peer book actor that owns the
//! address book, and bundles up the handles to it.

use std::{cmp::max, net::SocketAddr, sync::Arc, time::Instant};

use indexmap::IndexMap;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::Level;

use crate::{
    address_book::AddressMetrics,
    peer_book::{self, BanKey, ChangeSender, PeerBookHandle, PeerBookReader},
    AddressBook, BoxError, Config,
};

#[cfg(test)]
mod tests;

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

/// The handles to a spawned peer book actor.
///
/// The actor task exits with an error when every [`ChangeSender`] and
/// [`PeerBookHandle`] is dropped.
pub struct PeerBookHandles {
    /// A watch channel for the currently banned addresses.
    pub bans_receiver: watch::Receiver<Arc<IndexMap<BanKey, Instant>>>,

    /// The transmission channel for address book update events.
    pub change_sender: ChangeSender,

    /// A watch channel for address book metrics.
    pub address_metrics: watch::Receiver<AddressMetrics>,

    /// The peer book actor task.
    pub actor_task: JoinHandle<Result<(), BoxError>>,

    /// A handle for requests answered by the book.
    pub handle: PeerBookHandle,

    /// A reader serving lock-free peer book reads.
    pub reader: PeerBookReader,
}

impl AddressBookUpdater {
    /// Spawn the peer book actor, which owns a new [`AddressBook`]
    /// configured with Zebra's actual `local_listener` address.
    pub fn spawn(config: &Config, local_listener: SocketAddr) -> PeerBookHandles {
        let address_book = AddressBook::new(
            local_listener,
            &config.network,
            config.max_connections_per_ip,
            span!(Level::TRACE, "address book"),
        );

        // The channel is sized by the maximum number of inbound and outbound
        // peers.
        Self::spawn_with_book(
            address_book,
            local_listener,
            max(config.peerset_total_connection_limit(), MIN_CHANNEL_SIZE),
        )
    }

    /// Spawns the peer book actor owning `address_book`, announcing
    /// `local_listener`, with a message channel of `channel_size`.
    ///
    /// The actor's message channel carries both changes and calls, so the
    /// order between a connection event and a later request is preserved.
    pub(crate) fn spawn_with_book(
        address_book: AddressBook,
        local_listener: SocketAddr,
        channel_size: usize,
    ) -> PeerBookHandles {
        let (messages_tx, messages_rx) = mpsc::channel(channel_size);
        let address_metrics = address_book.address_metrics_watcher();

        let (bans_sender, bans_receiver) =
            watch::channel(Arc::new(IndexMap::<BanKey, Instant>::new()));
        let (recently_live_sender, recently_live_receiver) = watch::channel(Arc::new(Vec::new()));

        let change_sender = ChangeSender::new(messages_tx.clone());
        let peer_book_handle = PeerBookHandle::new(messages_tx);
        let peer_book_reader = PeerBookReader::new(
            recently_live_receiver,
            address_metrics.clone(),
            bans_receiver.clone(),
            change_sender.clone(),
            local_listener,
        );

        #[cfg(feature = "progress-bar")]
        let actor_task_handle = peer_book::spawn_actor(
            address_book,
            messages_rx,
            bans_sender,
            recently_live_sender,
            address_metrics.clone(),
        );
        #[cfg(not(feature = "progress-bar"))]
        let actor_task_handle =
            peer_book::spawn_actor(address_book, messages_rx, bans_sender, recently_live_sender);

        PeerBookHandles {
            bans_receiver,
            change_sender,
            address_metrics,
            actor_task: actor_task_handle,
            handle: peer_book_handle,
            reader: peer_book_reader,
        }
    }
}
