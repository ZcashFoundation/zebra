//! The `AddressBook` manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{cmp::Reverse, iter::Extend, net::SocketAddr, time::Instant};

use chrono::Utc;
use ordered_map::OrderedMap;
use tokio::sync::watch;
use tracing::Span;

use crate::{
    constants, meta_addr::MetaAddrChange, protocol::external::canonical_socket_addr,
    types::MetaAddr, PeerAddrState,
};

#[cfg(test)]
mod tests;

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
#[derive(Debug)]
pub struct AddressBook {
    /// Peer listener addresses, suitable for outbound connections,
    /// in connection attempt order.
    ///
    /// Some peers in this list might have open outbound or inbound connections.
    ///
    /// We reverse the comparison order, because the standard library ([`BTreeMap`])
    /// sorts in ascending order, but [`OrderedMap`] sorts in descending order.
    by_addr: OrderedMap<SocketAddr, MetaAddr, Reverse<MetaAddr>>,

    /// The maximum number of addresses in the address book.
    ///
    /// Always set to [`MAX_ADDRS_IN_ADDRESS_BOOK`](constants::MAX_ADDRS_IN_ADDRESS_BOOK),
    /// in release builds. Lower values are used during testing.
    addr_limit: usize,

    /// The local listener address.
    local_listener: SocketAddr,

    /// The span for operations on this address book.
    span: Span,

    /// A channel used to send the latest address book metrics.
    address_metrics_tx: watch::Sender<AddressMetrics>,

    /// The last time we logged a message about the address metrics.
    last_address_log: Option<Instant>,
}

/// Metrics about the states of the addresses in an [`AddressBook`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
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
    /// Construct an [`AddressBook`] with the given `local_listener` and
    /// [`tracing::Span`].
    pub fn new(local_listener: SocketAddr, span: Span) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        // The default value is correct for an empty address book,
        // and it gets replaced by `update_metrics` anyway.
        let (address_metrics_tx, _address_metrics_rx) = watch::channel(AddressMetrics::default());

        let mut new_book = AddressBook {
            by_addr: OrderedMap::new(|meta_addr| Reverse(*meta_addr)),
            addr_limit: constants::MAX_ADDRS_IN_ADDRESS_BOOK,
            local_listener: canonical_socket_addr(local_listener),
            span,
            address_metrics_tx,
            last_address_log: None,
        };

        new_book.update_metrics(instant_now, chrono_now);
        new_book
    }

    /// Construct an [`AddressBook`] with the given `local_listener`,
    /// `addr_limit`, [`tracing::Span`], and addresses.
    ///
    /// `addr_limit` is enforced by this method, and by [`AddressBook::update`].
    ///
    /// If there are multiple [`MetaAddr`]s with the same address,
    /// an arbitrary address is inserted into the address book,
    /// and the rest are dropped.
    ///
    /// This constructor can be used to break address book invariants,
    /// so it should only be used in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn new_with_addrs(
        local_listener: SocketAddr,
        addr_limit: usize,
        span: Span,
        addrs: impl IntoIterator<Item = MetaAddr>,
    ) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        let mut new_book = AddressBook::new(local_listener, span);
        new_book.addr_limit = addr_limit;

        let addrs = addrs
            .into_iter()
            .map(|mut meta_addr| {
                meta_addr.addr = canonical_socket_addr(meta_addr.addr);
                meta_addr
            })
            .filter(MetaAddr::address_is_valid_for_outbound)
            .take(addr_limit)
            .map(|meta_addr| (meta_addr.addr, meta_addr));

        for (socket_addr, meta_addr) in addrs {
            // overwrite any duplicate addresses
            new_book.by_addr.insert(socket_addr, meta_addr);
        }

        new_book.update_metrics(instant_now, chrono_now);
        new_book
    }

    /// Return a watch channel for the address book metrics.
    ///
    /// The metrics in the watch channel are only updated when the address book updates,
    /// so they can be significantly outdated if Zebra is disconnected or hung.
    ///
    /// The current metrics value is marked as seen.
    /// So `Receiver::changed` will only return after the next address book update.
    pub fn address_metrics_watcher(&self) -> watch::Receiver<AddressMetrics> {
        self.address_metrics_tx.subscribe()
    }

    /// Get the local listener address.
    ///
    /// This address contains minimal state, but it is not sanitized.
    pub fn local_listener_meta_addr(&self) -> MetaAddr {
        MetaAddr::new_local_listener_change(&self.local_listener)
            .into_new_meta_addr()
            .expect("unexpected invalid new local listener addr")
    }

    /// Get the contents of `self` in random order with sanitized timestamps.
    pub fn sanitized(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let _guard = self.span.enter();

        let mut peers = self.by_addr.clone();

        // Unconditionally add our local listener address to the advertised peers,
        // to replace any self-connection failures. The address book and change
        // constructors make sure that the SocketAddr is canonical.
        let local_listener = self.local_listener_meta_addr();
        peers.insert(local_listener.addr, local_listener);

        // Then sanitize and shuffle
        let mut peers = peers
            .descending_values()
            .filter_map(MetaAddr::sanitize)
            // Security: remove peers that:
            //   - last responded more than three hours ago, or
            //   - haven't responded yet but were reported last seen more than three hours ago
            //
            // This prevents Zebra from gossiping nodes that are likely unreachable. Gossiping such
            // nodes impacts the network health, because connection attempts end up being wasted on
            // peers that are less likely to respond.
            .filter(|addr| addr.is_active_for_gossip(now))
            .collect::<Vec<_>>();
        peers.shuffle(&mut rand::thread_rng());
        peers
    }

    /// Look up `addr` in the address book, and return its [`MetaAddr`].
    ///
    /// Converts `addr` to a canonical address before looking it up.
    pub fn get(&mut self, addr: &SocketAddr) -> Option<MetaAddr> {
        let addr = canonical_socket_addr(*addr);

        // Unfortunately, `OrderedMap` doesn't implement `get`.
        let meta_addr = self.by_addr.remove(&addr);

        if let Some(meta_addr) = meta_addr {
            self.by_addr.insert(addr, meta_addr);
        }

        meta_addr
    }

    /// Apply `change` to the address book, returning the updated `MetaAddr`,
    /// if the change was valid.
    ///
    /// # Correctness
    ///
    /// All changes should go through `update`, so that the address book
    /// only contains valid outbound addresses.
    ///
    /// Change addresses must be canonical `SocketAddr`s. This makes sure that
    /// each address book entry has a unique IP address.
    ///
    /// # Security
    ///
    /// This function must apply every attempted, responded, and failed change
    /// to the address book. This prevents rapid reconnections to the same peer.
    ///
    /// As an exception, this function can ignore all changes for specific
    /// [`SocketAddr`]s. Ignored addresses will never be used to connect to
    /// peers.
    pub fn update(&mut self, change: MetaAddrChange) -> Option<MetaAddr> {
        let previous = self.get(&change.addr());

        let _guard = self.span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        let updated = change.apply_to_meta_addr(previous);

        debug!(
            ?change,
            ?updated,
            ?previous,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers(chrono_now).count(),
            "calculated updated address book entry",
        );

        if let Some(updated) = updated {
            // Ignore invalid outbound addresses.
            // (Inbound connections can be monitored via Zebra's metrics.)
            if !updated.address_is_valid_for_outbound() {
                return None;
            }

            // Ignore invalid outbound services and other info,
            // but only if the peer has never been attempted.
            //
            // Otherwise, if we got the info directly from the peer,
            // store it in the address book, so we know not to reconnect.
            //
            // TODO: delete peers with invalid info when they get too old (#1873)
            if !updated.last_known_info_is_valid_for_outbound()
                && updated.last_connection_state.is_never_attempted()
            {
                return None;
            }

            self.by_addr.insert(updated.addr, updated);

            debug!(
                ?change,
                ?updated,
                ?previous,
                total_peers = self.by_addr.len(),
                recent_peers = self.recently_live_peers(chrono_now).count(),
                "updated address book entry",
            );

            // Security: Limit the number of peers in the address book.
            //
            // We only delete outdated peers when we have too many peers.
            // If we deleted them as soon as they became too old,
            // then other peers could re-insert them into the address book.
            // And we would start connecting to those outdated peers again,
            // ignoring the age limit in [`MetaAddr::is_probably_reachable`].
            while self.by_addr.len() > self.addr_limit {
                let surplus_peer = self
                    .peers()
                    .next_back()
                    .expect("just checked there is at least one peer");

                self.by_addr.remove(&surplus_peer.addr);

                debug!(
                    surplus = ?surplus_peer,
                    ?updated,
                    total_peers = self.by_addr.len(),
                    recent_peers = self.recently_live_peers(chrono_now).count(),
                    "removed surplus address book entry",
                );
            }

            assert!(self.len() <= self.addr_limit);

            std::mem::drop(_guard);
            self.update_metrics(instant_now, chrono_now);
        }

        updated
    }

    /// Removes the entry with `addr`, returning it if it exists
    ///
    /// # Note
    ///
    /// All address removals should go through `take`, so that the address
    /// book metrics are accurate.
    #[allow(dead_code)]
    fn take(&mut self, removed_addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        trace!(
            ?removed_addr,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers(chrono_now).count(),
        );

        if let Some(entry) = self.by_addr.remove(&removed_addr) {
            std::mem::drop(_guard);
            self.update_metrics(instant_now, chrono_now);
            Some(entry)
        } else {
            None
        }
    }

    /// Returns true if the given [`SocketAddr`] is pending a reconnection
    /// attempt.
    pub fn pending_reconnection_addr(&mut self, addr: &SocketAddr) -> bool {
        let meta_addr = self.get(addr);

        let _guard = self.span.enter();
        match meta_addr {
            None => false,
            Some(peer) => peer.last_connection_state == PeerAddrState::AttemptPending,
        }
    }

    /// Return an iterator over all peers.
    ///
    /// Returns peers in reconnection attempt order, including recently connected peers.
    pub fn peers(&'_ self) -> impl Iterator<Item = MetaAddr> + DoubleEndedIterator + '_ {
        let _guard = self.span.enter();
        self.by_addr.descending_values().cloned()
    }

    /// Return an iterator over peers that are due for a reconnection attempt,
    /// in reconnection attempt order.
    pub fn reconnection_peers(
        &'_ self,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> impl Iterator<Item = MetaAddr> + DoubleEndedIterator + '_ {
        let _guard = self.span.enter();

        // Skip live peers, and peers pending a reconnect attempt.
        // The peers are already stored in sorted order.
        self.by_addr
            .descending_values()
            .filter(move |peer| peer.is_ready_for_connection_attempt(instant_now, chrono_now))
            .cloned()
    }

    /// Return an iterator over all the peers in `state`,
    /// in reconnection attempt order, including recently connected peers.
    pub fn state_peers(
        &'_ self,
        state: PeerAddrState,
    ) -> impl Iterator<Item = MetaAddr> + DoubleEndedIterator + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .descending_values()
            .filter(move |peer| peer.last_connection_state == state)
            .cloned()
    }

    /// Return an iterator over peers that might be connected,
    /// in reconnection attempt order.
    pub fn maybe_connected_peers(
        &'_ self,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> impl Iterator<Item = MetaAddr> + DoubleEndedIterator + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .descending_values()
            .filter(move |peer| !peer.is_ready_for_connection_attempt(instant_now, chrono_now))
            .cloned()
    }

    /// Return an iterator over peers we've seen recently,
    /// in reconnection attempt order.
    pub fn recently_live_peers(
        &'_ self,
        now: chrono::DateTime<Utc>,
    ) -> impl Iterator<Item = MetaAddr> + DoubleEndedIterator + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .descending_values()
            .filter(move |peer| peer.was_recently_live(now))
            .cloned()
    }

    /// Returns the number of entries in this address book.
    pub fn len(&self) -> usize {
        self.by_addr.len()
    }

    /// Returns metrics for the addresses in this address book.
    /// Only for use in tests.
    ///
    /// # Correctness
    ///
    /// Use [`AddressBook::address_metrics_watcher().borrow()`] in production code,
    /// to avoid deadlocks.
    #[cfg(test)]
    pub fn address_metrics(&self, now: chrono::DateTime<Utc>) -> AddressMetrics {
        self.address_metrics_internal(now)
    }

    /// Returns metrics for the addresses in this address book.
    ///
    /// # Correctness
    ///
    /// External callers should use [`AddressBook::address_metrics_watcher().borrow()`]
    /// in production code, to avoid deadlocks.
    /// (Using the watch channel receiver does not lock the address book mutex.)
    fn address_metrics_internal(&self, now: chrono::DateTime<Utc>) -> AddressMetrics {
        let responded = self.state_peers(PeerAddrState::Responded).count();
        let never_attempted_gossiped = self
            .state_peers(PeerAddrState::NeverAttemptedGossiped)
            .count();
        let never_attempted_alternate = self
            .state_peers(PeerAddrState::NeverAttemptedAlternate)
            .count();
        let failed = self.state_peers(PeerAddrState::Failed).count();
        let attempt_pending = self.state_peers(PeerAddrState::AttemptPending).count();

        let recently_live = self.recently_live_peers(now).count();
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
    fn update_metrics(&mut self, instant_now: Instant, chrono_now: chrono::DateTime<Utc>) {
        let _guard = self.span.enter();

        let m = self.address_metrics_internal(chrono_now);

        // Ignore errors: we don't care if any receivers are listening.
        let _ = self.address_metrics_tx.send(m);

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
        self.log_metrics(&m, instant_now);
    }

    /// Log metrics for this address book
    fn log_metrics(&mut self, m: &AddressMetrics, now: Instant) {
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
            if now.saturating_duration_since(last_address_log).as_secs() < 60 {
                return;
            }
        } else {
            // Suppress initial logs until the peer set has started up.
            // There can be multiple address changes before the first peer has
            // responded.
            self.last_address_log = Some(now);
            return;
        }

        self.last_address_log = Some(now);
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

impl Clone for AddressBook {
    /// Clone the addresses, address limit, local listener address, and span.
    ///
    /// Cloned address books have a separate metrics struct watch channel, and an empty last address log.
    ///
    /// All address books update the same prometheus metrics.
    fn clone(&self) -> AddressBook {
        // The existing metrics might be outdated, but we avoid calling `update_metrics`,
        // so we don't overwrite the prometheus metrics from the main address book.
        let (address_metrics_tx, _address_metrics_rx) =
            watch::channel(*self.address_metrics_tx.borrow());

        AddressBook {
            by_addr: self.by_addr.clone(),
            addr_limit: self.addr_limit,
            local_listener: self.local_listener,
            span: self.span.clone(),
            address_metrics_tx,
            last_address_log: None,
        }
    }
}
