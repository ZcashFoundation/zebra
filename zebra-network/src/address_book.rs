//! The `AddressBook` manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    cmp::Reverse,
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Instant,
};

use chrono::Utc;
use indexmap::IndexMap;
use ordered_map::OrderedMap;
use tokio::sync::watch;
use tracing::Span;

use zebra_chain::{parameters::Network, serialization::DateTime32};

use crate::{
    constants::{self, ADDR_RESPONSE_LIMIT_DENOMINATOR, MAX_ADDRS_IN_MESSAGE},
    meta_addr::MetaAddrChange,
    protocol::external::{canonical_peer_addr, canonical_socket_addr},
    types::MetaAddr,
    AddressBookPeers, PeerAddrState, PeerSocketAddr,
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
    /// We reverse the comparison order, because the standard library
    /// ([`BTreeMap`](std::collections::BTreeMap)) sorts in ascending order, but
    /// [`OrderedMap`] sorts in descending order.
    by_addr: OrderedMap<PeerSocketAddr, MetaAddr, Reverse<MetaAddr>>,

    /// The address with a last_connection_state of [`PeerAddrState::Responded`] and
    /// the most recent `last_response` time by IP.
    ///
    /// This is used to avoid initiating outbound connections past [`Config::max_connections_per_ip`](crate::config::Config), and
    /// currently only supports a `max_connections_per_ip` of 1, and must be `None` when used with a greater `max_connections_per_ip`.
    // TODO: Replace with `by_ip: HashMap<IpAddr, BTreeMap<DateTime32, MetaAddr>>` to support configured `max_connections_per_ip` greater than 1
    most_recent_by_ip: Option<HashMap<IpAddr, MetaAddr>>,

    /// A list of banned addresses, with the time they were banned.
    bans_by_ip: Arc<IndexMap<IpAddr, Instant>>,

    /// The local listener address.
    local_listener: SocketAddr,

    /// The configured Zcash network.
    network: Network,

    /// The maximum number of addresses in the address book.
    ///
    /// Always set to [`MAX_ADDRS_IN_ADDRESS_BOOK`](constants::MAX_ADDRS_IN_ADDRESS_BOOK),
    /// in release builds. Lower values are used during testing.
    addr_limit: usize,

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
    pub responded: usize,

    /// The number of addresses in the `NeverAttemptedGossiped` state.
    pub never_attempted_gossiped: usize,

    /// The number of addresses in the `Failed` state.
    pub failed: usize,

    /// The number of addresses in the `AttemptPending` state.
    pub attempt_pending: usize,

    /// The number of `Responded` addresses within the liveness limit.
    pub recently_live: usize,

    /// The number of `Responded` addresses outside the liveness limit.
    pub recently_stopped_responding: usize,

    /// The number of addresses in the address book, regardless of their states.
    pub num_addresses: usize,

    /// The maximum number of addresses in the address book.
    pub address_limit: usize,
}

#[allow(clippy::len_without_is_empty)]
impl AddressBook {
    /// Construct an [`AddressBook`] with the given `local_listener` on `network`.
    ///
    /// Uses the supplied [`tracing::Span`] for address book operations.
    pub fn new(
        local_listener: SocketAddr,
        network: &Network,
        max_connections_per_ip: usize,
        span: Span,
    ) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        // The default value is correct for an empty address book,
        // and it gets replaced by `update_metrics` anyway.
        let (address_metrics_tx, _address_metrics_rx) = watch::channel(AddressMetrics::default());

        // Avoid initiating outbound handshakes when max_connections_per_ip is 1.
        let should_limit_outbound_conns_per_ip = max_connections_per_ip == 1;
        let mut new_book = AddressBook {
            by_addr: OrderedMap::new(|meta_addr| Reverse(*meta_addr)),
            local_listener: canonical_socket_addr(local_listener),
            network: network.clone(),
            addr_limit: constants::MAX_ADDRS_IN_ADDRESS_BOOK,
            span,
            address_metrics_tx,
            last_address_log: None,
            most_recent_by_ip: should_limit_outbound_conns_per_ip.then(HashMap::new),
            bans_by_ip: Default::default(),
        };

        new_book.update_metrics(instant_now, chrono_now);
        new_book
    }

    /// Construct an [`AddressBook`] with the given `local_listener`, `network`,
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
        network: &Network,
        max_connections_per_ip: usize,
        addr_limit: usize,
        span: Span,
        addrs: impl IntoIterator<Item = MetaAddr>,
    ) -> AddressBook {
        let constructor_span = span.clone();
        let _guard = constructor_span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        // The maximum number of addresses should be always greater than 0
        assert!(addr_limit > 0);

        let mut new_book = AddressBook::new(local_listener, network, max_connections_per_ip, span);
        new_book.addr_limit = addr_limit;

        let addrs = addrs
            .into_iter()
            .map(|mut meta_addr| {
                meta_addr.addr = canonical_peer_addr(meta_addr.addr);
                meta_addr
            })
            .filter(|meta_addr| meta_addr.address_is_valid_for_outbound(network))
            .map(|meta_addr| (meta_addr.addr, meta_addr));

        for (socket_addr, meta_addr) in addrs {
            // overwrite any duplicate addresses
            new_book.by_addr.insert(socket_addr, meta_addr);
            // Add the address to `most_recent_by_ip` if it has responded
            if new_book.should_update_most_recent_by_ip(meta_addr) {
                new_book
                    .most_recent_by_ip
                    .as_mut()
                    .expect("should be some when should_update_most_recent_by_ip is true")
                    .insert(socket_addr.ip(), meta_addr);
            }
            // exit as soon as we get enough addresses
            if new_book.by_addr.len() >= addr_limit {
                break;
            }
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

    /// Set the local listener address. Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn set_local_listener(&mut self, addr: SocketAddr) {
        self.local_listener = addr;
    }

    /// Get the local listener address.
    ///
    /// This address contains minimal state, but it is not sanitized.
    pub fn local_listener_meta_addr(&self, now: chrono::DateTime<Utc>) -> MetaAddr {
        let now: DateTime32 = now.try_into().expect("will succeed until 2038");

        MetaAddr::new_local_listener_change(self.local_listener)
            .local_listener_into_new_meta_addr(now)
    }

    /// Get the local listener [`SocketAddr`].
    pub fn local_listener_socket_addr(&self) -> SocketAddr {
        self.local_listener
    }

    /// Get the active addresses in `self` in random order with sanitized timestamps,
    /// including our local listener address.
    ///
    /// Limited to the number of peer addresses Zebra should give out per `GetAddr` request.
    pub fn fresh_get_addr_response(&self) -> Vec<MetaAddr> {
        let now = Utc::now();
        let mut peers = self.sanitized(now);
        let address_limit = peers.len().div_ceil(ADDR_RESPONSE_LIMIT_DENOMINATOR);
        peers.truncate(MAX_ADDRS_IN_MESSAGE.min(address_limit));

        peers
    }

    /// Get the active addresses in `self` in random order with sanitized timestamps,
    /// including our local listener address.
    pub(crate) fn sanitized(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let _guard = self.span.enter();

        let mut peers = self.by_addr.clone();

        // Unconditionally add our local listener address to the advertised peers,
        // to replace any self-connection failures. The address book and change
        // constructors make sure that the SocketAddr is canonical.
        let local_listener = self.local_listener_meta_addr(now);
        peers.insert(local_listener.addr, local_listener);

        // Then sanitize and shuffle
        let mut peers: Vec<MetaAddr> = peers
            .descending_values()
            .filter_map(|meta_addr| meta_addr.sanitize(&self.network))
            // # Security
            //
            // Remove peers that:
            //   - last responded more than three hours ago, or
            //   - haven't responded yet but were reported last seen more than three hours ago
            //
            // This prevents Zebra from gossiping nodes that are likely unreachable. Gossiping such
            // nodes impacts the network health, because connection attempts end up being wasted on
            // peers that are less likely to respond.
            .filter(|addr| addr.is_active_for_gossip(now))
            .collect();

        peers.shuffle(&mut rand::thread_rng());

        peers
    }

    /// Get the active addresses in `self`, in preferred caching order,
    /// excluding our local listener address.
    pub fn cacheable(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        let _guard = self.span.enter();

        let peers = self.by_addr.clone();

        // Get peers in preferred order, then keep the recently active ones
        peers
            .descending_values()
            // # Security
            //
            // Remove peers that:
            //   - last responded more than three hours ago, or
            //   - haven't responded yet but were reported last seen more than three hours ago
            //
            // This prevents Zebra from caching nodes that are likely unreachable,
            // which improves startup time and reliability.
            .filter(|addr| addr.is_active_for_gossip(now))
            .cloned()
            .collect()
    }

    /// Look up `addr` in the address book, and return its [`MetaAddr`].
    ///
    /// Converts `addr` to a canonical address before looking it up.
    pub fn get(&mut self, addr: PeerSocketAddr) -> Option<MetaAddr> {
        let addr = canonical_peer_addr(*addr);

        // Unfortunately, `OrderedMap` doesn't implement `get`.
        let meta_addr = self.by_addr.remove(&addr);

        if let Some(meta_addr) = meta_addr {
            self.by_addr.insert(addr, meta_addr);
        }

        meta_addr
    }

    /// Returns true if `updated` needs to be applied to the recent outbound peer connection IP cache.
    ///
    /// Checks if there are no existing entries in the address book with this IP,
    /// or if `updated` has a more recent `last_response` requiring the outbound connector to wait
    /// longer before initiating handshakes with peers at this IP.
    ///
    /// This code only needs to check a single cache entry, rather than the entire address book,
    /// because other code maintains these invariants:
    /// - `last_response` times for an entry can only increase.
    /// - this is the only field checked by `has_connection_recently_responded()`
    ///
    /// See [`AddressBook::is_ready_for_connection_attempt_with_ip`] for more details.
    fn should_update_most_recent_by_ip(&self, updated: MetaAddr) -> bool {
        let Some(most_recent_by_ip) = self.most_recent_by_ip.as_ref() else {
            return false;
        };

        if let Some(previous) = most_recent_by_ip.get(&updated.addr.ip()) {
            updated.last_connection_state == PeerAddrState::Responded
                && updated.last_response() > previous.last_response()
        } else {
            updated.last_connection_state == PeerAddrState::Responded
        }
    }

    /// Returns true if `addr` is the latest entry for its IP, which is stored in `most_recent_by_ip`.
    /// The entry is checked for an exact match to the IP and port of `addr`.
    fn should_remove_most_recent_by_ip(&self, addr: PeerSocketAddr) -> bool {
        let Some(most_recent_by_ip) = self.most_recent_by_ip.as_ref() else {
            return false;
        };

        if let Some(previous) = most_recent_by_ip.get(&addr.ip()) {
            previous.addr == addr
        } else {
            false
        }
    }

    /// Apply `change` to the address book, returning the updated `MetaAddr`,
    /// if the change was valid.
    ///
    /// # Correctness
    ///
    /// All changes should go through `update`, so that the address book
    /// only contains valid outbound addresses.
    ///
    /// Change addresses must be canonical `PeerSocketAddr`s. This makes sure that
    /// each address book entry has a unique IP address.
    ///
    /// # Security
    ///
    /// This function must apply every attempted, responded, and failed change
    /// to the address book. This prevents rapid reconnections to the same peer.
    ///
    /// As an exception, this function can ignore all changes for specific
    /// [`PeerSocketAddr`]s. Ignored addresses will never be used to connect to
    /// peers.
    #[allow(clippy::unwrap_in_result)]
    pub fn update(&mut self, change: MetaAddrChange) -> Option<MetaAddr> {
        if self.bans_by_ip.contains_key(&change.addr().ip()) {
            tracing::warn!(
                ?change,
                "attempted to add a banned peer addr to address book"
            );
            return None;
        }

        let previous = self.get(change.addr());

        let _guard = self.span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        let updated = change.apply_to_meta_addr(previous, instant_now, chrono_now);

        trace!(
            ?change,
            ?updated,
            ?previous,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers(chrono_now).len(),
            "calculated updated address book entry",
        );

        if let Some(updated) = updated {
            if updated.misbehavior() >= constants::MAX_PEER_MISBEHAVIOR_SCORE {
                // Ban and skip outbound connections with excessively misbehaving peers.
                let banned_ip = updated.addr.ip();
                let bans_by_ip = Arc::make_mut(&mut self.bans_by_ip);

                bans_by_ip.insert(banned_ip, Instant::now());
                if bans_by_ip.len() > constants::MAX_BANNED_IPS {
                    // Remove the oldest banned IP from the address book.
                    bans_by_ip.shift_remove_index(0);
                }

                self.most_recent_by_ip
                    .as_mut()
                    .expect("should be some when should_remove_most_recent_by_ip is true")
                    .remove(&banned_ip);

                let banned_addrs: Vec<_> = self
                    .by_addr
                    .descending_keys()
                    .skip_while(|addr| addr.ip() != banned_ip)
                    .take_while(|addr| addr.ip() == banned_ip)
                    .cloned()
                    .collect();

                for addr in banned_addrs {
                    self.by_addr.remove(&addr);
                }

                warn!(
                    ?updated,
                    total_peers = self.by_addr.len(),
                    recent_peers = self.recently_live_peers(chrono_now).len(),
                    "banned ip and removed banned peer addresses from address book",
                );

                return None;
            }

            // Ignore invalid outbound addresses.
            // (Inbound connections can be monitored via Zebra's metrics.)
            if !updated.address_is_valid_for_outbound(&self.network) {
                return None;
            }

            // Ignore invalid outbound services and other info,
            // but only if the peer has never been attempted.
            //
            // Otherwise, if we got the info directly from the peer,
            // store it in the address book, so we know not to reconnect.
            if !updated.last_known_info_is_valid_for_outbound(&self.network)
                && updated.last_connection_state.is_never_attempted()
            {
                return None;
            }

            self.by_addr.insert(updated.addr, updated);

            // Add the address to `most_recent_by_ip` if it sent the most recent
            // response Zebra has received from this IP.
            if self.should_update_most_recent_by_ip(updated) {
                self.most_recent_by_ip
                    .as_mut()
                    .expect("should be some when should_update_most_recent_by_ip is true")
                    .insert(updated.addr.ip(), updated);
            }

            debug!(
                ?change,
                ?updated,
                ?previous,
                total_peers = self.by_addr.len(),
                recent_peers = self.recently_live_peers(chrono_now).len(),
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

                // Check if this surplus peer's addr matches that in `most_recent_by_ip`
                // for this the surplus peer's ip to remove it there as well.
                if self.should_remove_most_recent_by_ip(surplus_peer.addr) {
                    self.most_recent_by_ip
                        .as_mut()
                        .expect("should be some when should_remove_most_recent_by_ip is true")
                        .remove(&surplus_peer.addr.ip());
                }

                debug!(
                    surplus = ?surplus_peer,
                    ?updated,
                    total_peers = self.by_addr.len(),
                    recent_peers = self.recently_live_peers(chrono_now).len(),
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
    fn take(&mut self, removed_addr: PeerSocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();

        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        trace!(
            ?removed_addr,
            total_peers = self.by_addr.len(),
            recent_peers = self.recently_live_peers(chrono_now).len(),
        );

        if let Some(entry) = self.by_addr.remove(&removed_addr) {
            // Check if this surplus peer's addr matches that in `most_recent_by_ip`
            // for this the surplus peer's ip to remove it there as well.
            if self.should_remove_most_recent_by_ip(entry.addr) {
                if let Some(most_recent_by_ip) = self.most_recent_by_ip.as_mut() {
                    most_recent_by_ip.remove(&entry.addr.ip());
                }
            }

            std::mem::drop(_guard);
            self.update_metrics(instant_now, chrono_now);
            Some(entry)
        } else {
            None
        }
    }

    /// Returns true if the given [`PeerSocketAddr`] is pending a reconnection
    /// attempt.
    pub fn pending_reconnection_addr(&mut self, addr: PeerSocketAddr) -> bool {
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
    pub fn peers(&'_ self) -> impl DoubleEndedIterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();
        self.by_addr.descending_values().cloned()
    }

    /// Is this IP ready for a new outbound connection attempt?
    /// Checks if the outbound connection with the most recent response at this IP has recently responded.
    ///
    /// Note: last_response times may remain live for a long time if the local clock is changed to an earlier time.
    fn is_ready_for_connection_attempt_with_ip(
        &self,
        ip: &IpAddr,
        chrono_now: chrono::DateTime<Utc>,
    ) -> bool {
        let Some(most_recent_by_ip) = self.most_recent_by_ip.as_ref() else {
            // if we're not checking IPs, any connection is allowed
            return true;
        };
        let Some(same_ip_peer) = most_recent_by_ip.get(ip) else {
            // If there's no entry for this IP, any connection is allowed
            return true;
        };
        !same_ip_peer.has_connection_recently_responded(chrono_now)
    }

    /// Return an iterator over peers that are due for a reconnection attempt,
    /// in reconnection attempt order.
    pub fn reconnection_peers(
        &'_ self,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> impl DoubleEndedIterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        // Skip live peers, and peers pending a reconnect attempt.
        // The peers are already stored in sorted order.
        self.by_addr
            .descending_values()
            .filter(move |peer| {
                peer.is_ready_for_connection_attempt(instant_now, chrono_now, &self.network)
                    && self.is_ready_for_connection_attempt_with_ip(&peer.addr.ip(), chrono_now)
            })
            .cloned()
    }

    /// Return an iterator over all the peers in `state`,
    /// in reconnection attempt order, including recently connected peers.
    pub fn state_peers(
        &'_ self,
        state: PeerAddrState,
    ) -> impl DoubleEndedIterator<Item = MetaAddr> + '_ {
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
    ) -> impl DoubleEndedIterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();

        self.by_addr
            .descending_values()
            .filter(move |peer| {
                !peer.is_ready_for_connection_attempt(instant_now, chrono_now, &self.network)
            })
            .cloned()
    }

    /// Returns banned IP addresses.
    pub fn bans(&self) -> Arc<IndexMap<IpAddr, Instant>> {
        self.bans_by_ip.clone()
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
        let failed = self.state_peers(PeerAddrState::Failed).count();
        let attempt_pending = self.state_peers(PeerAddrState::AttemptPending).count();

        let recently_live = self.recently_live_peers(now).len();
        let recently_stopped_responding = responded
            .checked_sub(recently_live)
            .expect("all recently live peers must have responded");

        let num_addresses = self.len();

        AddressMetrics {
            responded,
            never_attempted_gossiped,
            failed,
            attempt_pending,
            recently_live,
            recently_stopped_responding,
            num_addresses,
            address_limit: self.addr_limit,
        }
    }

    /// Update the metrics for this address book.
    fn update_metrics(&mut self, instant_now: Instant, chrono_now: chrono::DateTime<Utc>) {
        let _guard = self.span.enter();

        let m = self.address_metrics_internal(chrono_now);

        // Ignore errors: we don't care if any receivers are listening.
        let _ = self.address_metrics_tx.send(m);

        // TODO: rename to address_book.[state_name]
        metrics::gauge!("candidate_set.responded").set(m.responded as f64);
        metrics::gauge!("candidate_set.gossiped").set(m.never_attempted_gossiped as f64);
        metrics::gauge!("candidate_set.failed").set(m.failed as f64);
        metrics::gauge!("candidate_set.pending").set(m.attempt_pending as f64);

        // TODO: rename to address_book.responded.recently_live
        metrics::gauge!("candidate_set.recently_live").set(m.recently_live as f64);
        // TODO: rename to address_book.responded.stopped_responding
        metrics::gauge!("candidate_set.disconnected").set(m.recently_stopped_responding as f64);

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
        if m.responded + m.attempt_pending + m.never_attempted_gossiped == 0 {
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

impl AddressBookPeers for AddressBook {
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        let _guard = self.span.enter();

        self.by_addr
            .descending_values()
            .filter(|peer| peer.was_recently_live(now))
            .cloned()
            .collect()
    }

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        if self.get(peer).is_some() {
            // Peer already exists in the address book, so we don't need to add it again.
            return false;
        }
        self.update(MetaAddr::new_initial_peer(peer)).is_some()
    }
}

impl AddressBookPeers for Arc<Mutex<AddressBook>> {
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        self.lock()
            .expect("panic in a previous thread that was holding the mutex")
            .recently_live_peers(now)
    }

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        self.lock()
            .expect("panic in a previous thread that was holding the mutex")
            .add_peer(peer)
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
            local_listener: self.local_listener,
            network: self.network.clone(),
            addr_limit: self.addr_limit,
            span: self.span.clone(),
            address_metrics_tx,
            last_address_log: None,
            most_recent_by_ip: self.most_recent_by_ip.clone(),
            bans_by_ip: self.bans_by_ip.clone(),
        }
    }
}
