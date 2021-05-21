use std::{
    cmp::{max, min},
    net::SocketAddr,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use tokio::time::{sleep_until, Instant};
use tower::Service;

use crate::{
    address_book::{spawn_blocking, AddressMetrics},
    constants, peer_set,
    types::MetaAddr,
    AddressBook, BoxError, Request, Response,
};

/// The [`CandidateSet`] manages outbound peer connection attempts.
/// Successful connections become peers in the [`PeerSet`].
///
/// The candidate set divides the set of all possible outbound peers into
/// disjoint subsets, using the [`PeerAddrState`]:
///
/// 1. [`Responded`] peers, which we previously had outbound connections to.
/// 2. [`NeverAttemptedSeed`] peers, which we learned about from our seed config,
///     but have never connected to;
/// 3. [`NeverAttemptedGossiped`] peers, which we learned about from other peers
///     but have never connected to;
/// 4. [`NeverAttemptedAlternate`] peers, which we learned from the [`Version`]
///     messages of directly connected peers, but have never connected to;
/// 5. [`Failed`] peers, to whom we attempted to connect but were unable to;
/// 6. [`AttemptPending`] peers, which we've recently queued for a connection.
///
/// Never attempted peers are always available for connection.
///
/// If a peer's attempted, success, or failed time is recent
/// (within the liveness limit), we avoid reconnecting to it.
/// Otherwise, we assume that it has disconnected or hung,
/// and attempt reconnection.
///
///
/// ```ascii,no_run
///                         ┌──────────────────┐
///                         │   Config / DNS   │
///             ┌───────────│       Seed       │───────────┐
///             │           │    Addresses     │           │
///             │           └──────────────────┘           │
///             │                    │ untrusted_last_seen │
///             │                    │     is unknown      │
///             ▼                    │                     ▼
///    ┌──────────────────┐          │          ┌──────────────────┐
///    │    Handshake     │          │          │     Peer Set     │
///    │    Canonical     │──────────┼──────────│     Gossiped     │
///    │    Addresses     │          │          │    Addresses     │
///    └──────────────────┘          │          └──────────────────┘
///     untrusted_last_seen          │                provides
///         set to now               │           untrusted_last_seen
///                                  ▼
///                                  Λ     ignore changes if ever attempted
///                                 ╱ ╲  (including `Responded` and `Failed`)
///                                ▕   ▏    otherwise, if never attempted:
///                                 ╲ ╱          update services only
///                                  V
///  ┌───────────────────────────────┼───────────────────────────────┐
///  │ AddressBook                   │                               │
///  │ disjoint `PeerAddrState`s     ▼                               │
///  │ ┌─────────────┐  ┌─────────────────────────┐  ┌─────────────┐ │
///  │ │ `Responded` │  │  `NeverAttemptedSeed`   │  │  `Failed`   │ │
/// ┌┼▶│    Peers    │  │`NeverAttemptedGossiped` │  │   Peers     │◀┼┐
/// ││ │             │  │`NeverAttemptedAlternate`│  │             │ ││
/// ││ │             │  │          Peers          │  │             │ ││
/// ││ └─────────────┘  └─────────────────────────┘  └─────────────┘ ││
/// ││        │                      │                      │        ││
/// ││ #1 oldest_first        #2 newest_first        #3 oldest_first ││
/// ││        ├──────────────────────┴──────────────────────┘        ││
/// ││        ▼                                                      ││
/// ││        Λ                                                      ││
/// ││       ╱ ╲            filter by                                ││
/// ││      ▕   ▏      !recently_used_addr                           ││
/// ││       ╲ ╱    to remove recent `Responded`,                    ││
/// ││        V  `AttemptPending`, and `Failed` peers                ││
/// ││        │                                                      ││
/// ││        │    try outbound connection,                          ││
/// ││        ▼ update last_attempted to now()                       ││
/// ││┌────────────────┐                                             ││
/// │││`AttemptPending`│                                             ││
/// │││     Peers      │                                             ││
/// │││                │                                             ││
/// ││└────────────────┘                                             ││
/// │└────────┼──────────────────────────────────────────────────────┘│
/// │         ▼                                                       │
/// │         Λ                                                       │
/// │        ╱ ╲                                                      │
/// │       ▕   ▏─────────────────────────────────────────────────────┘
/// │        ╲ ╱   connection failed, update last_failed to now()
/// │         V       (`AttemptPending` and `Responded` peers)
/// │         │
/// │         │ connection succeeded
/// │         ▼
/// │  ┌────────────┐
/// │  │    send    │
/// │  │peer::Client│
/// │  │to Discover │
/// │  └────────────┘
/// │         │
/// │         ▼
/// │┌───────────────────────────────────────┐
/// ││ every time we receive a peer message: │
/// └│  * update state to `Responded`        │
///  │  * update last_success to now()       │
///  └───────────────────────────────────────┘
/// ```
// TODO:
//   * show all possible transitions between Attempt/Responded/Failed,
//     except Failed -> Responded is invalid, must go through Attempt
//   * show that seed peers that transition to other never attempted
//     states are already in the address book
#[derive(Clone, Debug)]
pub(super) struct CandidateSet<P>
where
    P: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    P::Future: Send + 'static,
{
    pub(super) address_book: Arc<std::sync::Mutex<AddressBook>>,
    pub(super) peer_service: tower::timeout::Timeout<P>,
    next_peer_sleep_until: Arc<futures::lock::Mutex<Instant>>,
}

impl<P> CandidateSet<P>
where
    P: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    P::Future: Send + 'static,
{
    /// Uses `address_book` and `peer_service` to manage a [`CandidateSet`] of peers.
    pub fn new(
        address_book: Arc<std::sync::Mutex<AddressBook>>,
        peer_service: P,
    ) -> CandidateSet<P> {
        CandidateSet {
            address_book,
            peer_service: tower::timeout::Timeout::new(peer_service, constants::GET_ADDR_TIMEOUT),
            next_peer_sleep_until: Arc::new(futures::lock::Mutex::new(Instant::now())),
        }
    }

    /// Update the peer set from the network, limiting the fanout to
    /// `fanout_limit`.
    ///
    /// Returns the number of new candidate peers after the update.
    ///
    /// Implementation:
    /// - Asks a few live [`Responded`] peers to send us more peers.
    /// - Processes peer responses, adding new peers in the
    ///   [`NeverAttemptedGossiped`] state.
    ///
    /// ## Correctness
    ///
    /// Pass the initial peer set size as `fanout_limit` during initialization,
    /// so that Zebra does not send duplicate requests to the same peer.
    ///
    /// The crawler exits when update returns an error, so it must only return
    /// errors on permanent failures.
    ///
    /// The handshaker sets up the peer message receiver so it also sends a
    /// [`Responded`] peer address update.
    ///
    /// [`report_failed`][Self::report_failed] puts peers into the [`Failed`] state.
    ///
    /// [`next`][Self::next] puts peers into the [`AttemptPending`] state.
    ///
    /// [`Responded`]: crate::PeerAddrState::Responded
    /// [`NeverAttemptedGossiped`]: crate::PeerAddrState::NeverAttemptedGossiped
    /// [`Failed`]: crate::PeerAddrState::Failed
    /// [`AttemptPending`]: crate::PeerAddrState::AttemptPending
    pub async fn update(&mut self, fanout_limit: usize) -> usize {
        let fanout_limit = min(fanout_limit, constants::MAX_GET_ADDR_FANOUT);
        self.update_fanout(fanout_limit).await
    }

    /// Update the peer set from the network, limiting the fanout to
    /// `fanout_limit`.
    ///
    /// See [`update_initial`][Self::update_initial]  for details.
    #[instrument(skip(self))]
    async fn update_fanout(&mut self, fanout_limit: usize) -> usize {
        let before_candidates = self.candidate_peer_count().await;

        // Opportunistically crawl the network on every update call to ensure
        // we're actively fetching peers. Continue independently of whether we
        // actually receive any peers, but always ask the network for more.
        debug!(?before_candidates, "sending GetPeers requests");

        let mut responses = peer_set::spawn_fanout(
            self.peer_service.clone(),
            Request::Peers,
            fanout_limit,
            constants::REQUEST_TIMEOUT,
        );

        // There's no need to spawn concurrent threads for each response,
        // because address book updates are not concurrent.
        while let Some(rsp) = responses.next().await {
            match rsp {
                Ok(Response::Peers(addrs)) => {
                    trace!(
                        addr_count = ?addrs.len(),
                        ?addrs,
                        "got response to GetPeers"
                    );
                    let addrs = validate_addrs(addrs, Utc::now());
                    self.send_addrs(addrs).await;
                }
                Err(e) => {
                    // since we do a fanout, and new updates are triggered by
                    // each demand, we can ignore errors in individual responses
                    trace!(?e, "got error in GetPeers request");
                }
                Ok(_) => unreachable!("Peers requests always return Peers responses"),
            }
        }

        // Calculate the change in the number of candidates.
        // Since the address book is concurrent, this can be negative.
        let after_candidates = self.candidate_peer_count().await;
        let new_candidates = after_candidates.saturating_sub(before_candidates);
        debug!(
            ?new_candidates,
            ?before_candidates,
            ?after_candidates,
            "updated candidate set"
        );

        new_candidates
    }

    /// Add new `addrs` to the address book.
    async fn send_addrs(&mut self, addrs: Vec<MetaAddr>) {
        debug!(addrs_len = ?addrs.len(),
               "sending validated addresses to the address book");
        trace!(?addrs, "full validated address list");

        // Turn the addresses into "new gossiped" changes
        let addrs = addrs.into_iter().map(MetaAddr::new_gossiped_change);

        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        //
        // Extend handles duplicate addresses internally.
        spawn_blocking(&self.address_book.clone(), |address_book| {
            address_book.extend(addrs)
        })
        .await;
    }

    /// Returns the next candidate for a connection attempt, if any are available.
    ///
    /// Returns peers in [`MetaAddr::cmp`] order, lowest first:
    /// - oldest [`Responded`] that are not live
    /// - [`NeverAttemptedSeed`], if any
    /// - newest [`NeverAttemptedGossiped`]
    /// - newest [`NeverAttemptedAlternate`]
    /// - oldest [`Failed`] that are not recent
    /// - oldest [`AttemptPending`] that are not recent
    ///
    /// Skips peers that have recently been attempted, connected, or failed.
    ///
    /// ## Correctness
    ///
    /// [`AttemptPending`] peers will become [`Responded`] if they respond, or
    /// become [`Failed`] if they time out or provide a bad response.
    ///
    /// Live [`Responded`] peers will stay live if they keep responding, or
    /// become a connection candidate if they stop responding.
    ///
    /// ## Security
    ///
    /// Zebra resists distributed denial of service attacks by making sure that
    /// new peer connections are initiated at least
    /// [`MIN_PEER_CONNECTION_INTERVAL`] apart.
    #[instrument(skip(self))]
    pub async fn next(&mut self) -> Option<MetaAddr> {
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        //
        // To avoid deadlocks, the critical section:
        // - must not acquire any other locks, and
        // - must not await any futures.
        //
        // To avoid hangs, any computation in the critical section should
        // be kept to a minimum.
        let next_peer = spawn_blocking(&self.address_book.clone(), |address_book| {
            // It's okay to return without sleeping here, because we're returning
            // `None`. We only need to sleep before yielding an address.
            let next_peer = address_book.next_candidate_peer()?;

            let change = MetaAddr::update_attempt(next_peer.addr);
            // if the update fails, we return, to avoid a possible infinite loop
            let updated_next_peer = address_book.update(change);
            debug!(
                ?next_peer,
                ?updated_next_peer,
                "updated next peer in address book"
            );
            updated_next_peer
        })
        .await?;

        // # Security
        //
        // Rate-limit new candidate connections to avoid denial of service.
        //
        // Update the deadline before sleeping, so we handle concurrent requests
        // correctly.
        //
        // # Correctness
        //
        // In this critical section, we hold the next sleep time mutex, blocking
        // the current async task. (There is one task per handshake.)
        //
        // To avoid deadlocks, the critical section:
        // - must not acquire any other locks, and
        // - must not await any other futures.
        let current_deadline;
        let now;
        {
            let mut next_sleep_until_guard = self.next_peer_sleep_until.lock().await;
            current_deadline = *next_sleep_until_guard;
            now = Instant::now();
            // If we recently had a connection, base the next time off our sleep
            // time. Otherwise, use the current time, to avoid large bursts.
            *next_sleep_until_guard =
                max(current_deadline, now) + constants::MIN_PEER_CONNECTION_INTERVAL;
        }

        // Now that the next deadline has been updated, actually do the sleep
        if current_deadline > now {
            debug!(?current_deadline,
                   ?now,
                   next_peer_sleep_until = ?self.next_peer_sleep_until,
                   ?next_peer,
                   "sleeping to rate-limit new peer connections");
        }
        sleep_until(current_deadline).await;
        debug!(?next_peer, "returning next candidate peer");

        Some(next_peer)
    }

    /// Mark `addr` as a failed peer.
    #[instrument(skip(self))]
    pub async fn report_failed(&mut self, addr: SocketAddr) {
        let addr = MetaAddr::update_failed(addr, None);
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        spawn_blocking(&self.address_book.clone(), move |address_book| {
            address_book.update(addr)
        })
        .await;
    }

    /// Return the number of candidate outbound peers.
    ///
    /// This number can change over time as recently used peers expire.
    pub async fn candidate_peer_count(&mut self) -> usize {
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        spawn_blocking(&self.address_book.clone(), |address_book| {
            address_book.candidate_peer_count()
        })
        .await
    }

    /// Return the number of recently live outbound peers.
    ///
    /// This number can change over time as responded peers expire.
    //
    // TODO: get this information from the peer set, which tracks
    // inbound connections as well as outbound connections (#1552)
    pub async fn recently_live_peer_count(&mut self) -> usize {
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        spawn_blocking(&self.address_book.clone(), |address_book| {
            address_book.recently_live_peers().count()
        })
        .await
    }

    /// Return the number of recently used outbound peers.
    ///
    /// This number can change over time as recently used peers expire.
    pub async fn recently_used_peer_count(&mut self) -> usize {
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        spawn_blocking(&self.address_book.clone(), |address_book| {
            address_book.recently_used_peers().count()
        })
        .await
    }

    /// Return the [`AddressBook`] metrics.
    ///
    /// # Performance
    ///
    /// Calculating these metrics can be expensive for large address books.
    /// Don't call this function more than once every few seconds.
    pub async fn address_metrics(&mut self) -> AddressMetrics {
        // # Correctness
        //
        // Briefly hold the address book threaded mutex on a blocking thread.
        spawn_blocking(&self.address_book.clone(), |address_book| {
            address_book.address_metrics()
        })
        .await
    }
}

/// Check new `addrs` before adding them to the address book.
///
/// `last_seen_limit` is the maximum permitted last seen time, typically
/// [`Utc::now`].
///
/// If the data in an address is invalid, this function can:
/// - modify the address data, or
/// - delete the address.
//
// TODO: re-enable this lint when last_seen_limit is used
#[allow(unused_variables)]
fn validate_addrs(
    addrs: impl IntoIterator<Item = MetaAddr>,
    last_seen_limit: DateTime<Utc>,
) -> Vec<MetaAddr> {
    // Note: The address book handles duplicate addresses internally,
    // so we don't need to de-duplicate addresses here.

    // TODO:
    // We should eventually implement these checks in this function:
    // - Zebra should stop believing far-future last_seen times from peers (#1871)
    // - Zebra should ignore peers that are older than 3 weeks (part of #1865)
    //   - Zebra should count back 3 weeks from the newest peer timestamp sent
    //     by the other peer, to compensate for clock skew
    // - Zebra should limit the number of addresses it uses from a single Addrs
    //   response (#1869)

    addrs.into_iter().collect()
}
