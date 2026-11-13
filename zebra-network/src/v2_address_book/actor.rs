//! The segment actor: one async task that owns the version 2 address book.
//!
//! The actor applies messages in channel order, and never awaits while it changes the book, so
//! no other task can observe the book mid-change, and the book needs no lock.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    net::SocketAddr,
    time::{Duration, Instant},
};

use chrono::Utc;
use rand::{seq::IndexedRandom, RngExt};
use tokio::sync::{mpsc, watch};
use zebra_chain::parameters::Network;

use crate::{
    constants::{MAX_ADDRS_IN_ADDRESS_BOOK, MAX_PEER_DISK_CACHE_SIZE, PEER_ADDR_RESPONSE_LIMIT},
    meta_addr::{MetaAddr, MetaAddrChange},
    protocol::external::canonical_peer_addr,
    AddressBook, BanList, PeerAddrState, PeerSocketAddr,
};

use super::{
    buckets::{GossipBuckets, Group, V2_GOSSIP_CAPACITY},
    Message, Provenance, V2BookStats, V2_DNS_RETRY_FAILED_AFTER,
};

/// How long one sanitized `get-addr` answer is served before a fresh sample is drawn, before
/// jitter.
///
/// Serving one snapshot stops repeated address requests from enumerating the segment: every
/// requester sees the same sample for the interval. These are the zcashd reference values.
pub(super) const GET_ADDR_SNAPSHOT_BASE_INTERVAL: Duration = Duration::from_secs(21 * 60 * 60);

/// The most random jitter added to the `get-addr` snapshot interval at each rotation.
pub(super) const GET_ADDR_SNAPSHOT_JITTER: Duration = Duration::from_secs(6 * 60 * 60);

/// The fewest addresses, other than this node's own, that a `get-addr` snapshot needs to be
/// kept for its interval. A smaller snapshot is drawn again at the next request.
///
/// The segment starts nearly empty after every restart, so without this, the first request
/// would freeze an answer with little more than this node's own address for about a day.
pub(super) const GET_ADDR_SNAPSHOT_MIN_ADDRS: usize = PEER_ADDR_RESPONSE_LIMIT / 4;

/// The most DNS seed and cached entries the segment trusts. Configured entries are always
/// trusted, on top of these. Past this, a new seed replaces a seeded entry that failed and never
/// answered.
pub(super) const V2_MAX_SEEDED: usize = 1000;

/// The most tried entries the segment keeps: past this, the entries that failed longest ago are
/// removed.
///
/// With [`V2_GOSSIP_CAPACITY`] relayed and [`V2_MAX_SEEDED`] seeded entries, this keeps the book
/// below [`MAX_ADDRS_IN_ADDRESS_BOOK`], so the book's own eviction, which ignores provenance,
/// never fires. Without it, relayed entries that were dialled and failed would leave the buckets
/// and pile up as tried entries, for as long as relay supplies new ones.
pub(super) const V2_MAX_TRIED: usize = 1500;

// A snapshot that is too small to keep is never truncated, so it has every address to serve.
const _: () = assert!(GET_ADDR_SNAPSHOT_MIN_ADDRS < PEER_ADDR_RESPONSE_LIMIT);

// The segment's populations fit in the book, with room for the configured peers.
const _: () =
    assert!(V2_GOSSIP_CAPACITY + V2_MAX_SEEDED + V2_MAX_TRIED < MAX_ADDRS_IN_ADDRESS_BOOK);

/// The segment's state.
pub(super) struct V2Book {
    /// The segment's entries.
    pub(super) book: AddressBook,

    /// The buckets that bound the relayed, never-attempted entries.
    pub(super) buckets: GossipBuckets,

    /// The trusted entries, which are never in the buckets.
    pub(super) trusted: HashMap<PeerSocketAddr, Provenance>,

    /// The current `get-addr` answer, and when it expires.
    get_addr_snapshot: Option<(tokio::time::Instant, Vec<MetaAddr>)>,

    /// The network the segment is for.
    pub(super) network: Network,

    /// This node's own advertised version 2 address.
    pub(super) local_listener: PeerSocketAddr,

    /// The latest ban list.
    pub(super) bans: BanList,

    /// Whether the endpoint can only reach IPv4 addresses, so IPv6 entries are never dialled.
    pub(super) ipv4_only: bool,

    /// Where the statistics are published.
    stats_tx: watch::Sender<V2BookStats>,
}

impl V2Book {
    /// Returns an empty segment for `network`, advertising `local_listener`, for an endpoint
    /// that can only reach IPv4 addresses if `ipv4_only`.
    pub(super) fn new(
        network: &Network,
        local_listener: SocketAddr,
        ipv4_only: bool,
        max_connections_per_ip: usize,
        bans: BanList,
        stats_tx: watch::Sender<V2BookStats>,
    ) -> Self {
        // The legacy address book's metrics and logs describe the legacy book only.
        let book = AddressBook::new_without_metrics(
            local_listener,
            network,
            max_connections_per_ip,
            tracing::info_span!("v2_address_book"),
        );
        V2Book {
            local_listener: canonical_peer_addr(book.local_listener_socket_addr()),
            book,
            buckets: GossipBuckets::new(),
            trusted: HashMap::new(),
            get_addr_snapshot: None,
            network: network.clone(),
            bans,
            ipv4_only,
            stats_tx,
        }
    }

    /// Handles one message, and returns whether it can have changed the book.
    pub(super) fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Change(change) => self.change(change, Instant::now()),
            Message::Intake { source, addrs, via } => self.intake(source, addrs, via),
            Message::Seed { addrs, provenance } => self.seed(addrs, provenance, Instant::now()),
            Message::NextCandidate {
                exclude,
                busy_groups,
                reply,
            } => {
                // The dialer may have given up, which leaves the book as it was.
                let _ = reply.send(self.next_candidate(&exclude, &busy_groups));
                return false;
            }
            Message::GetAddrSample { reply } => {
                let _ = reply.send(self.get_addr_sample(tokio::time::Instant::now()));
                return false;
            }
            Message::Cacheable { reply } => {
                let _ = reply.send(self.cacheable(Utc::now()));
                return false;
            }
        }
        true
    }

    /// Applies a connection status change of an outbound connection.
    pub(super) fn change(&mut self, change: MetaAddrChange, now: Instant) {
        if self.bans.is_banned(change.addr().ip()) {
            return;
        }
        let Some(updated) = self.book.update(change) else {
            return;
        };
        // The entry is tried now, so relay can no longer evict it.
        if updated.last_connection_state != PeerAddrState::NeverAttemptedGossiped {
            self.buckets.remove(updated.addr);
        }
        if updated.last_connection_state == PeerAddrState::Failed {
            self.limit_tried(now);
        }
    }

    /// Adds trusted addresses from `provenance`.
    ///
    /// An address the segment already has is only marked as trusted, unless its entry failed at
    /// least [`V2_DNS_RETRY_FAILED_AFTER`] before `now`, which makes it a candidate again.
    ///
    /// When the segment has [`V2_MAX_SEEDED`] DNS seed and cached entries, a new one replaces
    /// the seeded entry that failed longest ago without ever answering, and is dropped if there
    /// is none. Most DNS seed results do not accept QUIC, so without the replacement the failed
    /// ones would fill the segment, and later seeds would be dropped until a restart.
    pub(super) fn seed(
        &mut self,
        addrs: Vec<PeerSocketAddr>,
        provenance: Provenance,
        now: Instant,
    ) {
        let addrs: Vec<PeerSocketAddr> = addrs
            .into_iter()
            .map(|addr| canonical_peer_addr(*addr))
            .collect();
        let mut seeded = self.seeded_count();
        // The seeded entries that can make room, newest failure first. Listed once, when the
        // first seed finds the segment full.
        let mut evictable: Option<Vec<(Instant, PeerSocketAddr)>> = None;

        for &addr in &addrs {
            if self.bans.is_banned(addr.ip()) {
                continue;
            }
            let is_new_seed =
                provenance != Provenance::Configured && !self.trusted.contains_key(&addr);
            if is_new_seed && seeded >= V2_MAX_SEEDED {
                let evictable = evictable.get_or_insert_with(|| self.evictable_seeds(&addrs, now));
                let Some((_, evicted)) = evictable.pop() else {
                    metrics::counter!("zcash.net.v2.addresses.seeds.dropped").increment(1);
                    tracing::debug!(
                        ?addr,
                        ?provenance,
                        "dropped a v2 seed: the segment has {V2_MAX_SEEDED} seeded entries, \
                         and none of them can be evicted",
                    );
                    continue;
                };
                self.book.remove(evicted);
                self.trusted.remove(&evicted);
                seeded = seeded.saturating_sub(1);
            }

            let reseed = match self.book.get(addr) {
                None => true,
                Some(entry) => {
                    entry.last_connection_state == PeerAddrState::Failed
                        && entry.last_failure().is_some_and(|failed_at| {
                            now.saturating_duration_since(failed_at) >= V2_DNS_RETRY_FAILED_AFTER
                        })
                }
            };
            let in_book = if reseed {
                self.book.update(MetaAddr::new_initial_peer(addr)).is_some()
                    || self.book.get(addr).is_some()
            } else {
                self.book.get(addr).is_some()
            };
            if !in_book {
                continue;
            }

            self.buckets.remove(addr);
            match self.trusted.entry(addr) {
                Entry::Vacant(entry) => {
                    entry.insert(provenance);
                    if provenance != Provenance::Configured {
                        seeded = seeded.saturating_add(1);
                    }
                }
                // The operator's word outranks the other sources.
                Entry::Occupied(mut entry) => {
                    if provenance == Provenance::Configured
                        && entry.insert(Provenance::Configured) != Provenance::Configured
                    {
                        seeded = seeded.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// Returns the seeded entries that a new seed can replace, sorted so the one that failed
    /// longest ago is last: the DNS seed and cached entries whose last attempt failed and that
    /// never answered, other than the `incoming` seeds, which would only be added again.
    ///
    /// Configured entries, entries that answered, and entries that are waiting for their first
    /// attempt or have one in progress are never listed.
    fn evictable_seeds(
        &self,
        incoming: &[PeerSocketAddr],
        now: Instant,
    ) -> Vec<(Instant, PeerSocketAddr)> {
        let incoming: HashSet<&PeerSocketAddr> = incoming.iter().collect();
        let mut evictable: Vec<(Instant, PeerSocketAddr)> = self
            .book
            .state_peers(PeerAddrState::Failed)
            .filter(|peer| {
                peer.last_response().is_none()
                    && !incoming.contains(&peer.addr)
                    && self
                        .trusted
                        .get(&peer.addr)
                        .is_some_and(|provenance| *provenance != Provenance::Configured)
            })
            .map(|peer| (peer.last_failure().unwrap_or(now), peer.addr))
            .collect();
        evictable.sort_by_key(|entry| std::cmp::Reverse(*entry));
        evictable
    }

    /// Returns an entry due for an attempt that is not in `exclude`, nor in one of the
    /// `busy_groups`, nor banned, nor unreachable from the endpoint. The book is left as it is:
    /// the dialer marks the entry as attempted when it dials it.
    ///
    /// # Security
    ///
    /// Relayed `time`s and services are chosen by the relaying peers, so they never rank a
    /// candidate (draft, "Address Book": do not make the relayed `time` the primary key of
    /// outbound peer selection, nor bias it towards the addresses received most recently). The
    /// candidate is:
    /// 1. the entry this node itself heard from most recently, if one is due, because this
    ///    node's own record of a successful connection is the most trustworthy, then
    /// 2. a random never-attempted entry: a trusted one or a relayed one with equal odds, so
    ///    relay cannot crowd out the DNS seeds and the cache, however many relayed entries
    ///    there are, then
    /// 3. a random entry that was attempted, but never answered.
    pub(super) fn next_candidate(
        &self,
        exclude: &HashSet<PeerSocketAddr>,
        busy_groups: &HashSet<Group>,
    ) -> Option<MetaAddr> {
        self.next_candidate_at(exclude, busy_groups, Instant::now(), Utc::now())
    }

    /// [`V2Book::next_candidate`] at `instant_now` and `chrono_now`.
    pub(super) fn next_candidate_at(
        &self,
        exclude: &HashSet<PeerSocketAddr>,
        busy_groups: &HashSet<Group>,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> Option<MetaAddr> {
        let mut answered: Option<MetaAddr> = None;
        let mut trusted = Vec::new();
        let mut relayed = Vec::new();
        let mut unanswered = Vec::new();

        // The book's own ban list is empty: the segment's bans come from the shared ban list.
        for peer in self
            .book
            .reconnection_peers(instant_now, chrono_now)
            .filter(|peer| {
                !exclude.contains(&peer.addr)
                    && !busy_groups.contains(&Group::of(peer.addr.ip()))
                    && !self.bans.is_banned(peer.addr.ip())
                    && self.is_reachable(peer.addr)
            })
        {
            if peer.last_response().is_some() {
                if answered
                    .as_ref()
                    .is_none_or(|best| peer.last_response() > best.last_response())
                {
                    answered = Some(peer);
                }
            } else if peer.last_connection_state.is_never_attempted() {
                if self.trusted.contains_key(&peer.addr) {
                    trusted.push(peer);
                } else {
                    relayed.push(peer);
                }
            } else {
                unanswered.push(peer);
            }
        }
        if answered.is_some() {
            return answered;
        }

        let mut rng = rand::rng();
        let pool = match (trusted.is_empty(), relayed.is_empty()) {
            (false, false) if rng.random_bool(0.5) => trusted,
            (false, false) => relayed,
            (false, true) => trusted,
            (true, false) => relayed,
            (true, true) => unanswered,
        };
        pool.choose(&mut rng).cloned()
    }

    /// Returns the addresses to write to the version 2 peer cache at `now`: the entries this node
    /// heard from in the last [`MAX_PEER_ACTIVE_FOR_GOSSIP`](crate::constants::MAX_PEER_ACTIVE_FOR_GOSSIP),
    /// most recent first, at most [`MAX_PEER_DISK_CACHE_SIZE`].
    ///
    /// # Security
    ///
    /// The cache is loaded as trusted entries, so it only holds addresses this node connected
    /// to itself, and whose own init advertised `NODE_NETWORK` (draft, "Address Book": keep
    /// independently trusted addresses apart from relayed ones). A relayed entry that was never answered is never cached, whatever `time` its
    /// relaying peer claimed, so relay cannot become trusted across a restart.
    pub(super) fn cacheable(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        let mut cacheable: Vec<MetaAddr> = self
            .book
            .cacheable(now)
            .into_iter()
            .filter(|peer| peer.last_response().is_some())
            // Not redialled while this node runs, so not after a restart either.
            .filter(|peer| peer.last_known_info_is_valid_for_outbound(&self.network))
            .collect();
        cacheable.sort_by_key(|peer| std::cmp::Reverse(peer.last_response()));
        cacheable.truncate(MAX_PEER_DISK_CACHE_SIZE);
        cacheable
    }

    /// Returns whether the endpoint can dial `addr`.
    fn is_reachable(&self, addr: PeerSocketAddr) -> bool {
        !self.ipv4_only || !addr.is_ipv6()
    }

    /// Returns the current `get-addr` answer, drawing a new one when it expired at `now`.
    ///
    /// # Security
    ///
    /// One sanitized snapshot is served for the whole interval, so repeated requests cannot
    /// enumerate the segment. A snapshot with fewer than [`GET_ADDR_SNAPSHOT_MIN_ADDRS`]
    /// addresses other than this node's own is never kept: the requests after startup would
    /// otherwise get almost nothing for the interval while the segment fills. Drawing such a
    /// snapshot again reveals nothing more than one request does, because it is smaller than
    /// [`PEER_ADDR_RESPONSE_LIMIT`], so it already has every address the segment would serve.
    ///
    /// The snapshot is filled with this node's own address and the entries that answered this
    /// node first, and only then with entries that were only relayed, so relayed records cannot
    /// crowd verified addresses out of an answer.
    pub(super) fn get_addr_sample(&mut self, now: tokio::time::Instant) -> Vec<MetaAddr> {
        let local_listener = self.local_listener;
        let stale = self
            .get_addr_snapshot
            .as_ref()
            .is_none_or(|(deadline, addrs)| {
                now >= *deadline
                    || addrs
                        .iter()
                        .filter(|addr| addr.addr != local_listener)
                        .count()
                        < GET_ADDR_SNAPSHOT_MIN_ADDRS
            });
        if stale {
            // The sanitized entries are shuffled, and the partition keeps their order.
            let (mut addrs, relayed): (Vec<MetaAddr>, Vec<MetaAddr>) = self
                .book
                .sanitized(Utc::now())
                .into_iter()
                .partition(|addr| {
                    addr.addr == local_listener
                        || self
                            .book
                            .get(addr.addr)
                            .is_some_and(|entry| entry.last_response().is_some())
                });
            addrs.extend(relayed);
            addrs.truncate(PEER_ADDR_RESPONSE_LIMIT);
            let jitter = GET_ADDR_SNAPSHOT_JITTER.mul_f64(rand::random::<f64>());
            self.get_addr_snapshot = Some((now + GET_ADDR_SNAPSHOT_BASE_INTERVAL + jitter, addrs));
        }

        self.get_addr_snapshot
            .as_ref()
            .map(|(_, addrs)| addrs.clone())
            .expect("the snapshot was just filled if it was missing")
    }

    /// Removes every banned entry from the book, the buckets, and the trusted entries.
    pub(super) fn apply_bans(&mut self, bans: BanList) {
        self.bans = bans;
        let banned: Vec<PeerSocketAddr> = self
            .book
            .peers()
            .map(|peer| peer.addr)
            .filter(|addr| self.bans.is_banned(addr.ip()))
            .collect();
        for addr in banned {
            self.book.remove(addr);
        }
        let bans = &self.bans;
        self.buckets.remove_if(|addr| bans.is_banned(addr.ip()));
        self.trusted.retain(|addr, _| !bans.is_banned(addr.ip()));
        if let Some((_, addrs)) = &mut self.get_addr_snapshot {
            addrs.retain(|addr| !bans.is_banned(addr.addr.ip()));
        }
    }

    /// Removes the tried entries that failed longest ago, while there are more than
    /// [`V2_MAX_TRIED`].
    fn limit_tried(&mut self, now: Instant) {
        // Every entry that is not trusted is tried or relayed, so this bounds the tried entries
        // without a scan.
        if self.book.len().saturating_sub(self.trusted.len()) <= V2_MAX_TRIED {
            return;
        }

        let tried: Vec<MetaAddr> = self
            .book
            .peers()
            .filter(|peer| {
                !peer.last_connection_state.is_never_attempted()
                    && !self.trusted.contains_key(&peer.addr)
            })
            .collect();
        let Some(surplus) = tried.len().checked_sub(V2_MAX_TRIED) else {
            return;
        };

        let mut failed: Vec<(Instant, PeerSocketAddr)> = tried
            .iter()
            .filter(|peer| peer.last_connection_state == PeerAddrState::Failed)
            .map(|peer| (peer.last_failure().unwrap_or(now), peer.addr))
            .collect();
        failed.sort();
        for (_, addr) in failed.into_iter().take(surplus) {
            self.book.remove(addr);
        }
    }

    /// Returns the number of trusted entries that are not configured.
    fn seeded_count(&self) -> usize {
        self.trusted
            .values()
            .filter(|provenance| **provenance != Provenance::Configured)
            .count()
    }

    /// Returns the segment's statistics.
    pub(super) fn stats(&self) -> V2BookStats {
        let mut stats = V2BookStats {
            trusted: self.trusted.len(),
            ..V2BookStats::default()
        };
        for peer in self.book.peers() {
            let trusted = self.trusted.contains_key(&peer.addr);
            match peer.last_connection_state {
                PeerAddrState::NeverAttemptedGossiped if !trusted => stats.relayed += 1,
                state if !state.is_never_attempted() && !trusted => stats.tried += 1,
                _ => {}
            }
            if peer.last_connection_state != PeerAddrState::Failed
                && !self.bans.is_banned(peer.addr.ip())
                && self.is_reachable(peer.addr)
            {
                stats.dialable += 1;
            }
        }
        stats
    }

    /// Publishes the statistics if they changed, and checks the segment's size invariant.
    pub(super) fn publish_stats(&self) {
        let stats = self.stats();

        // The buckets, the seeded entries, and the tried entries are each bounded, so the book's
        // own eviction never fires. Only the configured peers, which the operator chooses, are
        // not.
        debug_assert!(
            V2_GOSSIP_CAPACITY + stats.trusted + stats.tried <= MAX_ADDRS_IN_ADDRESS_BOOK,
            "the v2 segment must stay below the address book limit: {stats:?}",
        );

        // The casts are safe: the counts are below the address book limit, far below 2^52.
        metrics::gauge!("zcash.net.v2.addresses.book", "kind" => "tried").set(stats.tried as f64);
        metrics::gauge!("zcash.net.v2.addresses.book", "kind" => "relayed")
            .set(stats.relayed as f64);
        metrics::gauge!("zcash.net.v2.addresses.book", "kind" => "trusted")
            .set(stats.trusted as f64);

        self.stats_tx.send_if_modified(|current| {
            let changed = *current != stats;
            *current = stats;
            changed
        });
    }
}

/// Runs the actor until every handle is dropped.
pub(super) async fn run(
    mut book: V2Book,
    mut messages: mpsc::Receiver<Message>,
    mut bans_receiver: watch::Receiver<BanList>,
) {
    let mut bans_open = true;
    book.publish_stats();

    loop {
        tokio::select! {
            changed = bans_receiver.changed(), if bans_open => {
                if changed.is_err() {
                    // The ban list is gone, so it never changes again: keep the last bans.
                    bans_open = false;
                    continue;
                }
                let bans = bans_receiver.borrow_and_update().clone();
                book.apply_bans(bans);
            }
            message = messages.recv() => {
                let Some(message) = message else {
                    tracing::debug!("every v2 address segment handle was dropped, stopping");
                    return;
                };
                // The statistics walk the whole book, so they are only computed again when it
                // can have changed.
                if !book.handle(message) {
                    continue;
                }
            }
        }
        book.publish_stats();
    }
}
