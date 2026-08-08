//! A cloneable, lock-free read handle for the peer book.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use chrono::Utc;
use indexmap::IndexMap;
use tokio::sync::watch;

use crate::{
    address_book::AddressMetrics, address_book_peers::AddressBookPeers, meta_addr::MetaAddr,
    peer_book::ChangeSender, PeerSocketAddr,
};

/// A cloneable, lock-free read handle for the peer book.
///
/// Reads are served from watch snapshots maintained by the actor, so they
/// never contend with the address book writer, and are safe in async
/// contexts.
///
/// The recently-live snapshot is a superset of the currently live peers,
/// refreshed by the actor on liveness-relevant changes with a short
/// debounce: reads filter it by `now`, so peers that went stale disappear
/// immediately, and newly live peers appear within about a second.
#[derive(Clone, Debug)]
pub struct PeerBookReader {
    /// A superset of the recently live peers, in reconnection attempt
    /// order, refreshed by the actor.
    recently_live: watch::Receiver<Arc<Vec<MetaAddr>>>,

    /// The address book metrics, updated by the address book itself.
    metrics: watch::Receiver<AddressMetrics>,

    /// The currently banned addresses, updated by the actor.
    bans: watch::Receiver<Arc<IndexMap<crate::peer_book::BanKey, Instant>>>,

    /// Sends changes to the actor, for [`AddressBookPeers::add_peer`].
    changes: ChangeSender,

    /// The local listener address, as bound.
    local_listener: SocketAddr,
}

impl PeerBookReader {
    /// Creates a reader over the actor's watch channels and change sender.
    pub(crate) fn new(
        recently_live: watch::Receiver<Arc<Vec<MetaAddr>>>,
        metrics: watch::Receiver<AddressMetrics>,
        bans: watch::Receiver<Arc<IndexMap<crate::peer_book::BanKey, Instant>>>,
        changes: ChangeSender,
        local_listener: SocketAddr,
    ) -> Self {
        PeerBookReader {
            recently_live,
            metrics,
            bans,
            changes,
            local_listener,
        }
    }

    /// Returns the current address book metrics.
    pub fn address_metrics(&self) -> AddressMetrics {
        *self.metrics.borrow()
    }

    /// Returns the currently banned addresses and their ban times.
    pub fn bans(&self) -> Arc<IndexMap<crate::peer_book::BanKey, Instant>> {
        self.bans.borrow().clone()
    }

    /// Returns the local listener address, as bound.
    pub fn local_listener_socket_addr(&self) -> SocketAddr {
        self.local_listener
    }

    /// Returns the reader's change sender.
    #[cfg(test)]
    pub(crate) fn change_sender(&self) -> &ChangeSender {
        &self.changes
    }
}

impl AddressBookPeers for PeerBookReader {
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        self.recently_live
            .borrow()
            .iter()
            .filter(|peer| peer.was_recently_live(now))
            .cloned()
            .collect()
    }

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        // The change is applied asynchronously by the actor, so the return
        // value reports whether the change was accepted for processing.
        // This method is only used to add peers on Regtest.
        self.changes
            .try_send(MetaAddr::new_initial_peer(peer))
            .is_ok()
    }
}
