//! The addressbook manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    collections::{BTreeSet, HashMap},
    iter::Extend,
    net::SocketAddr,
};

use chrono::{DateTime, Utc};
use tracing::Span;

use crate::{constants, types::MetaAddr, PeerConnectionState};

/// A database of peers, their advertised services, and information on when they
/// were last seen.
#[derive(Debug)]
pub struct AddressBook {
    /// Each known peer address has a matching `MetaAddr`
    by_addr: HashMap<SocketAddr, MetaAddr>,

    /// The span for operations on this address book.
    span: Span,
}

#[allow(clippy::len_without_is_empty)]
impl AddressBook {
    /// Construct an `AddressBook` with the given [`tracing::Span`].
    pub fn new(span: Span) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let new_book = AddressBook {
            by_addr: HashMap::default(),
            span,
        };

        new_book.update_metrics();
        new_book
    }

    /// Get the contents of `self` in random order with sanitized timestamps.
    pub fn sanitized(&self) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let _guard = self.span.enter();
        let mut peers = self.peers().map(MetaAddr::sanitize).collect::<Vec<_>>();
        peers.shuffle(&mut rand::thread_rng());
        peers
    }

    /// Returns true if the address book has an entry for `addr`.
    pub fn contains_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        self.by_addr.contains_key(addr)
    }

    /// Returns the entry corresponding to `addr`, or `None` if it does not exist.
    pub fn get_by_addr(&self, addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        self.by_addr.get(&addr).cloned()
    }

    /// Add `new` to the address book, updating the previous entry if `new` is
    /// more recent or discarding `new` if it is stale.
    ///
    /// ## Note
    ///
    /// All changes should go through `update` or `take`, to ensure accurate metrics.
    pub fn update(&mut self, new: MetaAddr) {
        let _guard = self.span.enter();
        trace!(
            ?new,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers().count(),
        );

        if let Some(prev) = self.get_by_addr(new.addr) {
            if prev.last_seen > new.last_seen {
                return;
            }
        }

        self.by_addr.insert(new.addr, new);
        self.update_metrics();
    }

    /// Removes the entry with `addr`, returning it if it exists
    ///
    /// ## Note
    ///
    /// All changes should go through `update` or `take`, to ensure accurate metrics.
    fn take(&mut self, removed_addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        trace!(
            ?removed_addr,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers().count(),
        );

        if let Some(entry) = self.by_addr.remove(&removed_addr) {
            self.update_metrics();
            Some(entry)
        } else {
            None
        }
    }

    /// Compute a cutoff time that can determine whether an entry
    /// in an address book being updated with peer message timestamps
    /// represents a likely-dead (or hung) peer, or a potentially-connected peer.
    ///
    /// [`constants::LIVE_PEER_DURATION`] represents the time interval in which
    /// we should receive at least one message from a peer, or close the
    /// connection. Therefore, if the last-seen timestamp is older than
    /// [`constants::LIVE_PEER_DURATION`] ago, we know we should have
    /// disconnected from it. Otherwise, we could potentially be connected to it.
    fn liveness_cutoff_time() -> DateTime<Utc> {
        // chrono uses signed durations while stdlib uses unsigned durations
        use chrono::Duration as CD;
        Utc::now() - CD::from_std(constants::LIVE_PEER_DURATION).unwrap()
    }

    /// Returns true if the given [`SocketAddr`] has recently sent us a message.
    pub fn recently_live_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(addr) {
            None => false,
            // NeverAttempted, Failed, and AttemptPending peers should never be live
            Some(peer) => {
                peer.last_connection_state == PeerConnectionState::Responded
                    && peer.last_seen > AddressBook::liveness_cutoff_time()
            }
        }
    }

    /// Returns true if the given [`SocketAddr`] is pending a reconnection
    /// attempt.
    pub fn pending_reconnection_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(addr) {
            None => false,
            Some(peer) => peer.last_connection_state == PeerConnectionState::AttemptPending,
        }
    }

    /// Returns true if the given [`SocketAddr`] might be connected to a node
    /// feeding timestamps into this address book.
    pub fn maybe_connected_addr(&self, addr: &SocketAddr) -> bool {
        self.recently_live_addr(addr) || self.pending_reconnection_addr(addr)
    }

    /// Return an iterator over all peers.
    ///
    /// Returns peers in reconnection attempt order, then recently live peers in
    /// an arbitrary order.
    pub fn peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();
        self.reconnection_peers()
            .chain(self.maybe_connected_peers())
    }

    /// Return an iterator over peers that are due for a reconnection attempt,
    /// in reconnection attempt order.
    pub fn reconnection_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        // TODO: optimise, if needed, or get rid of older peers

        // Skip live peers, and peers pending a reconnect attempt, then sort using BTreeSet
        self.by_addr
            .values()
            .filter(move |peer| !self.maybe_connected_addr(&peer.addr))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .cloned()
    }

    /// Return an iterator over all the peers in `state`, in arbitrary order.
    pub fn state_peers(
        &'_ self,
        state: PeerConnectionState,
    ) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .values()
            .filter(move |peer| peer.last_connection_state == state)
            .cloned()
    }

    /// Return an iterator over peers that might be connected, in arbitrary
    /// order.
    pub fn maybe_connected_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .values()
            .filter(move |peer| self.maybe_connected_addr(&peer.addr))
            .cloned()
    }

    /// Return an iterator over peers we've seen recently, in arbitrary order.
    pub fn recently_live_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .values()
            .filter(move |peer| self.recently_live_addr(&peer.addr))
            .cloned()
    }

    /// Returns an iterator that drains entries from the address book.
    ///
    /// Removes entries in reconnection attempt then arbitrary order,
    /// see [`peers`] for details.
    pub fn drain(&'_ mut self) -> impl Iterator<Item = MetaAddr> + '_ {
        Drain { book: self }
    }

    /// Returns the number of entries in this address book.
    pub fn len(&self) -> usize {
        self.by_addr.len()
    }

    /// Update the metrics for this address book.
    fn update_metrics(&self) {
        let _guard = self.span.enter();

        let responded = self.state_peers(PeerConnectionState::Responded).count();
        let never_attempted = self
            .state_peers(PeerConnectionState::NeverAttempted)
            .count();
        let failed = self.state_peers(PeerConnectionState::Failed).count();
        let pending = self
            .state_peers(PeerConnectionState::AttemptPending)
            .count();

        let recently_live = self.recently_live_peers().count();
        let recently_stopped_responding = responded
            .checked_sub(recently_live)
            .expect("all recently live peers must have responded");

        // TODO: rename to address_book.responded.recently_live
        metrics::gauge!("candidate_set.recently_live", recently_live as f64);
        // TODO: rename to address_book.responded.stopped_responding
        metrics::gauge!(
            "candidate_set.disconnected",
            recently_stopped_responding as f64
        );

        // TODO: rename to address_book.[state_name]
        metrics::gauge!("candidate_set.responded", responded as f64);
        metrics::gauge!("candidate_set.gossiped", never_attempted as f64);
        metrics::gauge!("candidate_set.failed", failed as f64);
        metrics::gauge!("candidate_set.pending", pending as f64);

        debug!(
            %recently_live,
            %recently_stopped_responding,
            %responded,
            %never_attempted,
            %failed,
            %pending,
            "address book peers"
        );
    }
}

impl Extend<MetaAddr> for AddressBook {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = MetaAddr>,
    {
        for meta in iter.into_iter() {
            self.update(meta);
        }
    }
}

struct Drain<'a> {
    book: &'a mut AddressBook,
}

impl<'a> Iterator for Drain<'a> {
    type Item = MetaAddr;

    fn next(&mut self) -> Option<Self::Item> {
        let next_item_addr = self.book.peers().next()?.addr;
        self.book.take(next_item_addr)
    }
}
