//! The [`AddressBook`] manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{collections::HashMap, iter::Extend, net::SocketAddr, sync::Arc, time::Instant};

use chrono::{DateTime, Utc};
use tracing::Span;

use crate::{constants, meta_addr::MetaAddrChange, types::MetaAddr, Config, PeerAddrState};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A database of peer listener addresses, their advertised services, and
/// information on when they were last seen.
///
/// # Security
///
/// Address book state must be based on outbound connections to peers.
///
/// If the address book is updated incorrectly:
/// - malicious peers can interfere with other peers' [`AddressBook`] state,
///   or
/// - Zebra can advertise unreachable addresses to its own peers.
///
/// ## Adding Addresses
///
/// The address book should only contain Zcash listener port addresses from peers
/// on the configured network. These addresses can come from:
/// - the initial seed peers config
/// - addresses gossiped by other peers
/// - the canonical address ([`Version.address_from`]) provided by each peer,
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
///
/// See the [`CandidateSet`] for a detailed peer state diagram.
#[derive(Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AddressBook {
    /// Each known peer address has a matching [`MetaAddr`].
    by_addr: HashMap<SocketAddr, MetaAddr>,

    /// The local listener address.
    local_listener: SocketAddr,

    /// The span for operations on this address book.
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(value = "Span::current()")
    )]
    span: Span,

    /// The last time we logged a message about the address metrics.
    last_address_log: Option<Instant>,
}

/// Metrics about the states of the addresses in an [`AddressBook`].
#[derive(Copy, Clone, Debug)]
pub struct AddressMetrics {
    /// The number of addresses in the [`Responded`] state.
    responded: usize,

    /// The number of addresses in the [`NeverAttemptedSeed`] state.
    never_attempted_seed: usize,

    /// The number of addresses in the [`NeverAttemptedGossiped`] state.
    never_attempted_gossiped: usize,

    /// The number of addresses in the [`NeverAttemptedAlternate`] state.
    never_attempted_alternate: usize,

    /// The number of addresses in the [`Failed`] state.
    failed: usize,

    /// The number of addresses in the [`AttemptPending`] state.
    attempt_pending: usize,

    /// The number of peers that we've tried to connect to recently.
    recently_attempted: usize,

    /// The number of peers that have recently sent us messages.
    recently_live: usize,

    /// The number of peers that have failed recently.
    recently_failed: usize,

    /// The number of peers that are connection candidates.
    connection_candidates: usize,
}

impl AddressBook {
    /// Construct an [`AddressBook`] with the given `config` and [`tracing::Span`].
    pub fn new(config: Config, span: Span) -> AddressBook {
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

    /// Returns a Change that adds or updates the local listener address in an
    /// [`AddressBook`].
    ///
    /// Our inbound listener port can be advertised to peers by applying this
    /// change to the inbound request address book.
    ///
    /// # Correctness
    ///
    /// Avoid inserting this address into the local [`AddressBook`].
    /// (If peers gossip our address back to us, the handshake nonce will
    /// protect us from self-connections.)
    pub fn get_local_listener(&self) -> MetaAddrChange {
        MetaAddr::new_local_listener(self.local_listener)
    }

    /// Get the contents of `self` in random order with sanitized timestamps.
    ///
    /// Skips peers with missing fields.
    pub fn sanitized(&self) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let _guard = self.span.enter();
        let mut peers = self
            .peers_unordered()
            .filter_map(|a| MetaAddr::sanitize(&a))
            .collect::<Vec<_>>();
        peers.shuffle(&mut rand::thread_rng());
        peers
    }

    /// Returns true if the address book has an entry for `addr`.
    pub fn contains_addr(&self, addr: SocketAddr) -> bool {
        let _guard = self.span.enter();
        self.by_addr.contains_key(&addr)
    }

    /// Returns the entry corresponding to `addr`, or [`None`] if it does not exist.
    pub fn get_by_addr(&self, addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        self.by_addr.get(&addr).cloned()
    }

    /// Apply `change` to the address book.
    ///
    /// If an entry was added or updated, return it.
    ///
    /// # Correctness
    ///
    /// All address book changes should go through `update`, so that the
    /// address book only contains valid outbound addresses.
    pub fn update(&mut self, change: MetaAddrChange) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        trace!(
            ?change,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers().count(),
        );
        let addr = change.get_addr();
        let book_entry = self.by_addr.get(&addr).cloned();

        // If a node that we are directly connected to has changed to a client,
        // remove it from the address book.
        if change.is_direct_client(book_entry) && self.contains_addr(addr) {
            std::mem::drop(_guard);
            self.take(addr);
            self.update_metrics();
            return None;
        }

        // Never add unspecified addresses or client services.
        //
        // Communication with these addresses can be monitored via Zebra's
        // metrics. (The address book is for valid peer addresses.)
        if !change.is_valid_for_outbound(book_entry) {
            return None;
        }

        let new_entry = change.into_meta_addr(book_entry);
        if let Some(new_entry) = new_entry {
            self.by_addr.insert(addr, new_entry);
            std::mem::drop(_guard);
            self.update_metrics();
            Some(new_entry)
        } else {
            None
        }
    }

    /// Removes the entry with `addr`, returning it if it exists
    ///
    /// # Note
    ///
    /// All address removals should go through [`take`], so that the address
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
        // chrono uses signed durations while stdlib uses unsigned durations
        use chrono::Duration as CD;
        Utc::now() - CD::from_std(constants::LIVE_PEER_DURATION).unwrap()
    }

    /// Returns true if the given [`SocketAddr`] has recently sent us a message.
    pub fn recently_live_addr(&self, addr: SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(&addr) {
            None => false,
            // Responded peers are the only peers that can be live
            Some(peer) => {
                peer.get_last_success().unwrap_or(chrono::MIN_DATETIME)
                    > AddressBook::liveness_cutoff_time()
            }
        }
    }

    /// Returns true if the given [`SocketAddr`] had a recent connection attempt.
    pub fn recently_attempted_addr(&self, addr: SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(&addr) {
            None => false,
            Some(peer) => {
                peer.get_last_attempt().unwrap_or(chrono::MIN_DATETIME)
                    > AddressBook::liveness_cutoff_time()
            }
        }
    }

    /// Returns true if the given [`SocketAddr`] recently failed.
    pub fn recently_failed_addr(&self, addr: SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(&addr) {
            None => false,
            Some(peer) => {
                peer.get_last_failed().unwrap_or(chrono::MIN_DATETIME)
                    > AddressBook::liveness_cutoff_time()
            }
        }
    }

    /// Returns true if the given [`SocketAddr`] had recent attempts, successes,
    /// or failures.
    pub fn recently_used_addr(&self, addr: SocketAddr) -> bool {
        self.recently_live_addr(addr)
            || self.recently_attempted_addr(addr)
            || self.recently_failed_addr(addr)
    }

    /// Return an unordered iterator over all peers.
    fn peers_unordered(&'_ self) -> impl Iterator<Item = &MetaAddr> + '_ {
        let _guard = self.span.enter();
        self.by_addr.values()
    }

    /// Return an iterator over peers that we've recently tried to connect to,
    /// in arbitrary order.
    fn recently_attempted_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.peers_unordered()
            .filter(move |peer| self.recently_attempted_addr(peer.addr))
            .cloned()
    }

    /// Return an iterator over peers that have recently sent us messages,
    /// in arbitrary order.
    pub fn recently_live_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.peers_unordered()
            .filter(move |peer| self.recently_live_addr(peer.addr))
            .cloned()
    }

    /// Return an iterator over peers that have recently failed,
    /// in arbitrary order.
    fn recently_failed_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.peers_unordered()
            .filter(move |peer| self.recently_failed_addr(peer.addr))
            .cloned()
    }

    /// Return an iterator over peers that had recent attempts, successes, or failures,
    /// in arbitrary order.
    pub fn recently_used_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.peers_unordered()
            .filter(move |peer| self.recently_used_addr(peer.addr))
            .cloned()
    }

    /// Return an iterator over candidate peers, in arbitrary order.
    ///
    /// Candidate peers have not had recent attempts, successes, or failures.
    fn candidate_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        // Skip recently used peers (including live peers)
        self.peers_unordered()
            .filter(move |peer| !self.recently_used_addr(peer.addr))
            .cloned()
    }

    /// Return the next peer that is due for a connection attempt.
    pub fn next_candidate_peer(&self) -> Option<MetaAddr> {
        let _guard = self.span.enter();

        self.candidate_peers().min()
    }

    /// Return the number of candidate peers.
    ///
    /// This number can change over time as recently used peers expire.
    pub fn candidate_peer_count(&self) -> usize {
        let _guard = self.span.enter();

        self.candidate_peers().count()
    }

    /// Returns the number of entries in this address book.
    pub fn len(&self) -> usize {
        self.by_addr.len()
    }

    /// Is this address book empty?
    pub fn is_empty(&self) -> bool {
        self.by_addr.is_empty()
    }

    /// Returns metrics for the addresses in this address book.
    pub fn address_metrics(&self) -> AddressMetrics {
        let responded = self
            .peers_unordered()
            .filter(|peer| matches!(peer.last_connection_state, PeerAddrState::Responded { .. }))
            .count();
        let never_attempted_seed = self
            .peers_unordered()
            .filter(|peer| {
                matches!(
                    peer.last_connection_state,
                    PeerAddrState::NeverAttemptedSeed
                )
            })
            .count();
        let never_attempted_gossiped = self
            .peers_unordered()
            .filter(|peer| {
                matches!(
                    peer.last_connection_state,
                    PeerAddrState::NeverAttemptedGossiped { .. }
                )
            })
            .count();
        let never_attempted_alternate = self
            .peers_unordered()
            .filter(|peer| {
                matches!(
                    peer.last_connection_state,
                    PeerAddrState::NeverAttemptedAlternate { .. }
                )
            })
            .count();
        let failed = self
            .peers_unordered()
            .filter(|peer| matches!(peer.last_connection_state, PeerAddrState::Failed { .. }))
            .count();
        let attempt_pending = self
            .peers_unordered()
            .filter(|peer| {
                matches!(
                    peer.last_connection_state,
                    PeerAddrState::AttemptPending { .. }
                )
            })
            .count();

        let recently_attempted = self.recently_attempted_peers().count();
        let recently_live = self.recently_live_peers().count();
        let recently_failed = self.recently_failed_peers().count();
        let connection_candidates = self.len() - self.recently_used_peers().count();

        AddressMetrics {
            responded,
            never_attempted_seed,
            never_attempted_gossiped,
            never_attempted_alternate,
            failed,
            attempt_pending,
            recently_attempted,
            recently_live,
            recently_failed,
            connection_candidates,
        }
    }

    /// Update the metrics for this address book.
    fn update_metrics(&mut self) {
        let _guard = self.span.enter();

        let m = self.address_metrics();

        // States
        // TODO: rename to address_book.[state_name]
        metrics::gauge!("candidate_set.seed", m.never_attempted_seed as f64);
        metrics::gauge!("candidate_set.gossiped", m.never_attempted_gossiped as f64);
        metrics::gauge!(
            "candidate_set.alternate",
            m.never_attempted_alternate as f64
        );
        metrics::gauge!("candidate_set.responded", m.responded as f64);
        metrics::gauge!("candidate_set.failed", m.failed as f64);
        metrics::gauge!("candidate_set.pending", m.attempt_pending as f64);

        // Times
        metrics::gauge!(
            "candidate_set.recently_attempted",
            m.recently_attempted as f64
        );
        metrics::gauge!("candidate_set.recently_live", m.recently_live as f64);
        metrics::gauge!("candidate_set.recently_failed", m.recently_failed as f64);

        // Candidates (state and time based)
        metrics::gauge!(
            "candidate_set.connection_candidates",
            m.connection_candidates as f64
        );

        std::mem::drop(_guard);
        self.log_metrics(m);
    }

    /// Log metrics for this address book
    fn log_metrics(&mut self, m: AddressMetrics) {
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

/// Run `f(address_book.lock())` in a thread dedicated to blocking tasks.
///
/// This avoids blocking code running concurrently in the same task, and other
/// async tasks on the same thread.
///
/// For details, see [`tokio::task::spawn_blocking`].
///
/// # Panics
///
/// If `f` panics, or if a previous task panicked while holding the mutex.
pub async fn spawn_blocking<F, R>(address_book: &Arc<std::sync::Mutex<AddressBook>>, f: F) -> R
where
    F: FnOnce(&mut AddressBook) -> R + Send + 'static,
    R: Send + 'static,
{
    let address_book = address_book.clone();
    let lock_query_fn = move || {
        let mut address_book_guard = address_book
            .lock()
            .expect("unexpected panic when mutex was previously locked");
        f(&mut address_book_guard)
    };

    tokio::task::spawn_blocking(lock_query_fn)
        .await
        .expect("unexpected panic in spawned address book task")
}
