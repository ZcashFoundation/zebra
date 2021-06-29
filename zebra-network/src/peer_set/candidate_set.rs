use std::{cmp::min, mem, sync::Arc, time::Duration};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::{sleep, timeout, Instant, Sleep};
use tower::{Service, ServiceExt};

use zebra_chain::serialization::DateTime32;

use crate::{constants, types::MetaAddr, AddressBook, BoxError, Request, Response};

#[cfg(test)]
mod tests;

/// The [`CandidateSet`] manages outbound peer connection attempts.
/// Successful connections become peers in the [`PeerSet`].
///
/// The candidate set divides the set of all possible outbound peers into
/// disjoint subsets, using the [`PeerAddrState`]:
///
/// 1. [`Responded`] peers, which we have had an outbound connection to.
/// 2. [`NeverAttemptedGossiped`] peers, which we learned about from other peers
///    but have never connected to.
/// 3. [`NeverAttemptedAlternate`] peers, canonical addresses which we learned
///    from the [`Version`] messages of inbound and outbound connections,
///    but have never connected to.
/// 4. [`Failed`] peers, which failed a connection attempt, or had an error
///    during an outbound connection.
/// 5. [`AttemptPending`] peers, which we've recently queued for a connection.
///
/// Never attempted peers are always available for connection.
///
/// If a peer's attempted, responded, or failure time is recent
/// (within the liveness limit), we avoid reconnecting to it.
/// Otherwise, we assume that it has disconnected or hung,
/// and attempt reconnection.
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
///                                  Λ   if attempted, responded, or failed:
///                                 ╱ ╲         ignore gossiped info
///                                ▕   ▏    otherwise, if never attempted:
///                                 ╲ ╱    skip updates to existing fields
///                                  V
///  ┌───────────────────────────────┼───────────────────────────────┐
///  │ AddressBook                   │                               │
///  │ disjoint `PeerAddrState`s     ▼                               │
///  │ ┌─────────────┐  ┌─────────────────────────┐  ┌─────────────┐ │
///  │ │ `Responded` │  │`NeverAttemptedGossiped` │  │  `Failed`   │ │
/// ┌┼▶│    Peers    │  │`NeverAttemptedAlternate`│  │   Peers     │◀┼┐
/// ││ │             │  │          Peers          │  │             │ ││
/// ││ └─────────────┘  └─────────────────────────┘  └─────────────┘ ││
/// ││        │                      │                      │        ││
/// ││ #1 oldest_first        #2 newest_first        #3 oldest_first ││
/// ││        ├──────────────────────┴──────────────────────┘        ││
/// ││        ▼                                                      ││
/// ││        Λ                                                      ││
/// ││       ╱ ╲              filter by                              ││
/// ││      ▕   ▏        is_ready_for_connection_attempt             ││
/// ││       ╲ ╱    to remove recent `Responded`,                    ││
/// ││        V  `AttemptPending`, and `Failed` peers                ││
/// ││        │                                                      ││
/// ││        │    try outbound connection,                          ││
/// ││        ▼  update last_attempt to now()                        ││
/// ││┌────────────────┐                                             ││
/// │││`AttemptPending`│                                             ││
/// │││     Peers      │                                             ││
/// ││└────────────────┘                                             ││
/// │└────────┼──────────────────────────────────────────────────────┘│
/// │         ▼                                                       │
/// │         Λ                                                       │
/// │        ╱ ╲                                                      │
/// │       ▕   ▏─────────────────────────────────────────────────────┘
/// │        ╲ ╱   connection failed, update last_failure to now()
/// │         V
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
///  │  * update last_response to now()      │
///  └───────────────────────────────────────┘
/// ```
// TODO:
//   * show all possible transitions between Attempt/Responded/Failed,
//     except Failed -> Responded is invalid, must go through Attempt
//   * for now, seed peers go straight to handshaking and responded,
//     but we'll fix that once we add the Seed state
// When we add the Seed state:
//   * show that seed peers that transition to other never attempted
//     states are already in the address book
pub(crate) struct CandidateSet<S> {
    pub(super) address_book: Arc<std::sync::Mutex<AddressBook>>,
    pub(super) peer_service: S,
    wait_next_handshake: Sleep,
    min_next_crawl: Instant,
}

impl<S> CandidateSet<S>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    /// Uses `address_book` and `peer_service` to manage a [`CandidateSet`] of peers.
    pub fn new(
        address_book: Arc<std::sync::Mutex<AddressBook>>,
        peer_service: S,
    ) -> CandidateSet<S> {
        CandidateSet {
            address_book,
            peer_service,
            wait_next_handshake: sleep(Duration::from_secs(0)),
            min_next_crawl: Instant::now(),
        }
    }

    /// Update the peer set from the network, using the default fanout limit.
    ///
    /// See [`update_initial`][Self::update_initial] for details.
    pub async fn update(&mut self) -> Result<(), BoxError> {
        self.update_timeout(None).await
    }

    /// Update the peer set from the network, limiting the fanout to
    /// `fanout_limit`.
    ///
    /// - Ask a few live [`Responded`] peers to send us more peers.
    /// - Process all completed peer responses, adding new peers in the
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
    /// ## Security
    ///
    /// This call is rate-limited to prevent sending a burst of repeated requests for new peer
    /// addresses. Each call will only update the [`CandidateSet`] if more time
    /// than [`MIN_PEER_GET_ADDR_INTERVAL`][constants::MIN_PEER_GET_ADDR_INTERVAL] has passed since
    /// the last call. Otherwise, the update is skipped.
    ///
    /// [`Responded`]: crate::PeerAddrState::Responded
    /// [`NeverAttemptedGossiped`]: crate::PeerAddrState::NeverAttemptedGossiped
    /// [`Failed`]: crate::PeerAddrState::Failed
    /// [`AttemptPending`]: crate::PeerAddrState::AttemptPending
    pub async fn update_initial(&mut self, fanout_limit: usize) -> Result<(), BoxError> {
        self.update_timeout(Some(fanout_limit)).await
    }

    /// Update the peer set from the network, limiting the fanout to
    /// `fanout_limit`, and imposing a timeout on the entire fanout.
    ///
    /// See [`update_initial`][Self::update_initial] for details.
    async fn update_timeout(&mut self, fanout_limit: Option<usize>) -> Result<(), BoxError> {
        // SECURITY
        //
        // Rate limit sending `GetAddr` messages to peers.
        if self.min_next_crawl <= Instant::now() {
            // CORRECTNESS
            //
            // Use a timeout to avoid deadlocks when there are no connected
            // peers, and:
            // - we're waiting on a handshake to complete so there are peers, or
            // - another task that handles or adds peers is waiting on this task
            //   to complete.
            if let Ok(fanout_result) =
                timeout(constants::REQUEST_TIMEOUT, self.update_fanout(fanout_limit)).await
            {
                fanout_result?;
            } else {
                // update must only return an error for permanent failures
                info!("timeout waiting for the peer service to become ready");
            }

            self.min_next_crawl = Instant::now() + constants::MIN_PEER_GET_ADDR_INTERVAL;
        }

        Ok(())
    }

    /// Update the peer set from the network, limiting the fanout to
    /// `fanout_limit`.
    ///
    /// See [`update_initial`][Self::update_initial]  for details.
    ///
    /// # Correctness
    ///
    /// This function does not have a timeout.
    /// Use [`update_timeout`][Self::update_timeout] instead.
    async fn update_fanout(&mut self, fanout_limit: Option<usize>) -> Result<(), BoxError> {
        // Opportunistically crawl the network on every update call to ensure
        // we're actively fetching peers. Continue independently of whether we
        // actually receive any peers, but always ask the network for more.
        //
        // Because requests are load-balanced across existing peers, we can make
        // multiple requests concurrently, which will be randomly assigned to
        // existing peers, but we don't make too many because update may be
        // called while the peer set is already loaded.
        let mut responses = FuturesUnordered::new();
        let fanout_limit = fanout_limit
            .map(|fanout_limit| min(fanout_limit, constants::GET_ADDR_FANOUT))
            .unwrap_or(constants::GET_ADDR_FANOUT);
        debug!(?fanout_limit, "sending GetPeers requests");
        // TODO: launch each fanout in its own task (might require tokio 1.6)
        for _ in 0..fanout_limit {
            let peer_service = self.peer_service.ready_and().await?;
            responses.push(peer_service.call(Request::Peers));
        }
        while let Some(rsp) = responses.next().await {
            match rsp {
                Ok(Response::Peers(addrs)) => {
                    trace!(
                        addr_count = ?addrs.len(),
                        ?addrs,
                        "got response to GetPeers"
                    );
                    let addrs = validate_addrs(addrs, DateTime32::now());
                    self.send_addrs(addrs);
                }
                Err(e) => {
                    // since we do a fanout, and new updates are triggered by
                    // each demand, we can ignore errors in individual responses
                    trace!(?e, "got error in GetPeers request");
                }
                Ok(_) => unreachable!("Peers requests always return Peers responses"),
            }
        }

        Ok(())
    }

    /// Add new `addrs` to the address book.
    fn send_addrs(&self, addrs: impl IntoIterator<Item = MetaAddr>) {
        let addrs = addrs.into_iter().map(MetaAddr::new_gossiped_change);

        // # Correctness
        //
        // Briefly hold the address book threaded mutex, to extend
        // the address list.
        //
        // Extend handles duplicate addresses internally.
        self.address_book.lock().unwrap().extend(addrs);
    }

    /// Returns the next candidate for a connection attempt, if any are available.
    ///
    /// Returns peers in reconnection order, based on
    /// [`AddressBook::reconnection_peers`].
    ///
    /// Skips peers that have recently been active, attempted, or failed.
    ///
    /// ## Correctness
    ///
    /// `AttemptPending` peers will become `Responded` if they respond, or
    /// become `Failed` if they time out or provide a bad response.
    ///
    /// Live `Responded` peers will stay live if they keep responding, or
    /// become a reconnection candidate if they stop responding.
    ///
    /// ## Security
    ///
    /// Zebra resists distributed denial of service attacks by making sure that
    /// new peer connections are initiated at least
    /// [`MIN_PEER_CONNECTION_INTERVAL`][constants::MIN_PEER_CONNECTION_INTERVAL] apart.
    pub async fn next(&mut self) -> Option<MetaAddr> {
        // # Correctness
        //
        // In this critical section, we hold the address mutex, blocking the
        // current thread, and all async tasks scheduled on that thread.
        //
        // To avoid deadlocks, the critical section:
        // - must not acquire any other locks
        // - must not await any futures
        //
        // To avoid hangs, any computation in the critical section should
        // be kept to a minimum.
        let reconnect = {
            let mut guard = self.address_book.lock().unwrap();
            // It's okay to return without sleeping here, because we're returning
            // `None`. We only need to sleep before yielding an address.
            let reconnect = guard.reconnection_peers().next()?;

            let reconnect = MetaAddr::new_reconnect(&reconnect.addr);
            guard.update(reconnect)?
        };

        // SECURITY: rate-limit new outbound peer connections
        (&mut self.wait_next_handshake).await;
        let mut sleep = sleep(constants::MIN_PEER_CONNECTION_INTERVAL);
        mem::swap(&mut self.wait_next_handshake, &mut sleep);

        Some(reconnect)
    }

    /// Mark `addr` as a failed peer.
    pub fn report_failed(&mut self, addr: &MetaAddr) {
        let addr = MetaAddr::new_errored(&addr.addr, addr.services);
        // # Correctness
        //
        // Briefly hold the address book threaded mutex, to update the state for
        // a single address.
        self.address_book.lock().unwrap().update(addr);
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
///
/// # Security
///
/// Adjusts untrusted last seen times so they are not in the future. This stops
/// malicious peers keeping all their addresses at the front of the connection
/// queue. Honest peers with future clock skew also get adjusted.
///
/// Rejects all addresses if any calculated times overflow or underflow.
fn validate_addrs(
    addrs: impl IntoIterator<Item = MetaAddr>,
    last_seen_limit: DateTime32,
) -> impl Iterator<Item = MetaAddr> {
    // Note: The address book handles duplicate addresses internally,
    // so we don't need to de-duplicate addresses here.

    // TODO:
    // We should eventually implement these checks in this function:
    // - Zebra should ignore peers that are older than 3 weeks (part of #1865)
    //   - Zebra should count back 3 weeks from the newest peer timestamp sent
    //     by the other peer, to compensate for clock skew
    // - Zebra should limit the number of addresses it uses from a single Addrs
    //   response (#1869)

    let mut addrs: Vec<_> = addrs.into_iter().collect();

    limit_last_seen_times(&mut addrs, last_seen_limit);

    addrs.into_iter()
}

/// Ensure all reported `last_seen` times are less than or equal to `last_seen_limit`.
///
/// This will consider all addresses as invalid if trying to offset their
/// `last_seen` times to be before the limit causes an underflow.
fn limit_last_seen_times(addrs: &mut Vec<MetaAddr>, last_seen_limit: DateTime32) {
    let last_seen_times = addrs.iter().map(|meta_addr| {
        meta_addr
            .untrusted_last_seen()
            .expect("unexpected missing last seen: should be provided by deserialization")
    });
    let oldest_seen = last_seen_times.clone().min().unwrap_or(DateTime32::MIN);
    let newest_seen = last_seen_times.max().unwrap_or(DateTime32::MAX);

    // If any time is in the future, adjust all times, to compensate for clock skew on honest peers
    if newest_seen > last_seen_limit {
        let offset = newest_seen
            .checked_duration_since(last_seen_limit)
            .expect("unexpected underflow: just checked newest_seen is greater");

        // Check for underflow
        if oldest_seen.checked_sub(offset).is_some() {
            // No underflow is possible, so apply offset to all addresses
            for addr in addrs {
                let last_seen = addr
                    .untrusted_last_seen()
                    .expect("unexpected missing last seen: should be provided by deserialization");
                let last_seen = last_seen
                    .checked_sub(offset)
                    .expect("unexpected underflow: just checked oldest_seen");

                addr.set_untrusted_last_seen(last_seen);
            }
        } else {
            // An underflow will occur, so reject all gossiped peers
            addrs.clear();
        }
    }
}
