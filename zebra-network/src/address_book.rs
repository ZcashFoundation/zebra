//! The `AddressBook` manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    collections::{BTreeSet, HashMap},
    iter::Extend,
    net::SocketAddr,
    time::Instant,
};

use chrono::{DateTime, Utc};
use tracing::Span;

use crate::{constants, meta_addr::MetaAddrChange, types::MetaAddr, Config, PeerAddrState};

/// A database of peer listener addresses, their advertised services, and
/// information on when they were last seen.
///
/// # Security
///
/// Address book state must be based on outbound connections to peers.
///
/// If the address book is updated incorrectly:
/// - malicious peers can interfere with other peers' `AddressBook` state,
///   or
/// - Zebra can advertise unreachable addresses to its own peers.
///
/// ## Adding Addresses
///
/// The address book should only contain Zcash listener port addresses from peers
/// on the configured network. These addresses can come from:
/// - DNS seeders
/// - addresses gossiped by other peers
/// - the canonical address (`Version.address_from`) provided by each peer,
///   particularly peers on inbound connections.
///
/// The remote addresses of inbound connections must not be added to the address
/// book, because they contain ephemeral outbound ports, not listener ports.
///
/// Isolated connections must not add addresses or update the address book.
///
/// ## Updating Address State
///
/// Updates to address state must be based on outbound connections to peers.
///
/// Updates must not be based on:
/// - the remote addresses of inbound connections, or
/// - the canonical address of any connection.
#[derive(Clone, Debug)]
pub struct AddressBook {
    /// Each known peer address has a matching `MetaAddr`.
    by_addr: HashMap<SocketAddr, MetaAddr>,

    /// The local listener address.
    local_listener: SocketAddr,

    /// The span for operations on this address book.
    span: Span,

    /// The last time we logged a message about the address metrics.
    last_address_log: Option<Instant>,
}

/// Metrics about the states of the addresses in an [`AddressBook`].
#[derive(Debug)]
pub struct AddressMetrics {
    /// The number of addresses in the `Responded` state.
    responded: usize,

    /// The number of addresses in the `NeverAttemptedGossiped` state.
    never_attempted_gossiped: usize,

    /// The number of addresses in the `NeverAttemptedAlternate` state.
    never_attempted_alternate: usize,

    /// The number of addresses in the `Failed` state.
    failed: usize,

    /// The number of addresses in the `AttemptPending` state.
    attempt_pending: usize,

    /// The number of `Responded` addresses within the liveness limit.
    recently_live: usize,

    /// The number of `Responded` addresses outside the liveness limit.
    recently_stopped_responding: usize,
}

#[allow(clippy::len_without_is_empty)]
impl AddressBook {
    /// Construct an `AddressBook` with the given `config` and [`tracing::Span`].
    pub fn new(config: &Config, span: Span) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let mut new_book = AddressBook {
            by_addr: HashMap::default(),
            local_listener: config.listen_addr,
            span,
            last_address_log: None,
        };

        new_book.update_metrics();
        new_book
    }

    /// Get the local listener address.
    pub fn get_local_listener(&self) -> MetaAddrChange {
        MetaAddr::new_local_listener(&self.local_listener)
    }

    /// Get the contents of `self` in random order with sanitized timestamps.
    pub fn sanitized(&self) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let _guard = self.span.enter();
        let mut peers = self
            .peers()
            .filter_map(|a| MetaAddr::sanitize(&a))
            .collect::<Vec<_>>();
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

    /// Apply `change` to the address book, returning the updated `MetaAddr`,
    /// if the change was valid.
    ///
    /// # Correctness
    ///
    /// All changes should go through `update`, so that the address book
    /// only contains valid outbound addresses.
    pub fn update(&mut self, change: MetaAddrChange) -> Option<MetaAddr> {
        let _guard = self.span.enter();

        let previous = self.by_addr.get(&change.addr()).cloned();
        let updated = change.apply_to_meta_addr(previous);

        trace!(
            ?change,
            ?updated,
            ?previous,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers().count(),
        );

        if let Some(updated) = updated {
            // If a node that we are directly connected to has changed to a client,
            // remove it from the address book.
            if updated.is_direct_client() && previous.is_some() {
                std::mem::drop(_guard);
                self.take(updated.addr);
                return None;
            }

            // Never add unspecified addresses or client services.
            //
            // Communication with these addresses can be monitored via Zebra's
            // metrics. (The address book is for valid peer addresses.)
            if !updated.is_valid_for_outbound() {
                return None;
            }

            self.by_addr.insert(updated.addr, updated);
            std::mem::drop(_guard);
            self.update_metrics();
        }

        updated
    }

    /// Removes the entry with `addr`, returning it if it exists
    ///
    /// # Note
    ///
    /// All address removals should go through `take`, so that the address
    /// book metrics are accurate.
    fn take(&mut self, removed_addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        trace!(
            ?removed_addr,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers().count(),
        );

        if let Some(entry) = self.by_addr.remove(&removed_addr) {
            std::mem::drop(_guard);
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
        Utc::now()
            - chrono::Duration::from_std(constants::LIVE_PEER_DURATION)
                .expect("unexpectedly large constant")
    }

    /// Returns true if the given [`SocketAddr`] has recently sent us a message.
    pub fn recently_live_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(addr) {
            None => false,
            // NeverAttempted, Failed, and AttemptPending peers should never be live
            Some(peer) => {
                peer.last_connection_state == PeerAddrState::Responded
                    && peer.get_last_seen() > AddressBook::liveness_cutoff_time()
            }
        }
    }

    /// Returns true if the given [`SocketAddr`] is pending a reconnection
    /// attempt.
    pub fn pending_reconnection_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(addr) {
            None => false,
            Some(peer) => peer.last_connection_state == PeerAddrState::AttemptPending,
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
    pub fn state_peers(&'_ self, state: PeerAddrState) -> impl Iterator<Item = MetaAddr> + '_ {
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

    /// Returns metrics for the addresses in this address book.
    pub fn address_metrics(&self) -> AddressMetrics {
        let responded = self.state_peers(PeerAddrState::Responded).count();
        let never_attempted_gossiped = self
            .state_peers(PeerAddrState::NeverAttemptedGossiped)
            .count();
        let never_attempted_alternate = self
            .state_peers(PeerAddrState::NeverAttemptedAlternate)
            .count();
        let failed = self.state_peers(PeerAddrState::Failed).count();
        let attempt_pending = self.state_peers(PeerAddrState::AttemptPending).count();

        let recently_live = self.recently_live_peers().count();
        let recently_stopped_responding = responded
            .checked_sub(recently_live)
            .expect("all recently live peers must have responded");

        AddressMetrics {
            responded,
            never_attempted_gossiped,
            never_attempted_alternate,
            failed,
            attempt_pending,
            recently_live,
            recently_stopped_responding,
        }
    }

    /// Update the metrics for this address book.
    fn update_metrics(&mut self) {
        let _guard = self.span.enter();

        let m = self.address_metrics();

        // TODO: rename to address_book.[state_name]
        metrics::gauge!("candidate_set.responded", m.responded as f64);
        metrics::gauge!("candidate_set.gossiped", m.never_attempted_gossiped as f64);
        metrics::gauge!(
            "candidate_set.alternate",
            m.never_attempted_alternate as f64
        );
        metrics::gauge!("candidate_set.failed", m.failed as f64);
        metrics::gauge!("candidate_set.pending", m.attempt_pending as f64);

        // TODO: rename to address_book.responded.recently_live
        metrics::gauge!("candidate_set.recently_live", m.recently_live as f64);
        // TODO: rename to address_book.responded.stopped_responding
        metrics::gauge!(
            "candidate_set.disconnected",
            m.recently_stopped_responding as f64
        );

        std::mem::drop(_guard);
        self.log_metrics(&m);
    }

    /// Log metrics for this address book
    fn log_metrics(&mut self, m: &AddressMetrics) {
        let _guard = self.span.enter();

        trace!(
            address_metrics = ?m,
        );

        if m.responded > 0 {
            return;
        }

        // These logs are designed to be human-readable in a terminal, at the
        // default Zebra log level. If you need to know address states for
        // every request, use the trace-level logs, or the metrics exporter.
        if let Some(last_address_log) = self.last_address_log {
            // Avoid duplicate address logs
            if Instant::now().duration_since(last_address_log).as_secs() < 60 {
                return;
            }
        } else {
            // Suppress initial logs until the peer set has started up.
            // There can be multiple address changes before the first peer has
            // responded.
            self.last_address_log = Some(Instant::now());
            return;
        }

        self.last_address_log = Some(Instant::now());
        // if all peers have failed
        if m.responded
            + m.attempt_pending
            + m.never_attempted_gossiped
            + m.never_attempted_alternate
            == 0
        {
            warn!(
                address_metrics = ?m,
                "all peer addresses have failed. Hint: check your network connection"
            );
        } else {
            info!(
                address_metrics = ?m,
                "no active peer connections: trying gossiped addresses"
            );
        }
    }
}

impl Extend<MetaAddrChange> for AddressBook {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = MetaAddrChange>,
    {
        for change in iter.into_iter() {
            self.update(change);
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
