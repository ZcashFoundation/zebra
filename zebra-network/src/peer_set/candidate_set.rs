//! Candidate peer selection for outbound connections.
//!
//! The crawler in [`crawl_and_dial`][crate::peer_set::initialize] manages
//! outbound peer connection attempts using the services in this module.
//! Successful connections become peers in the
//! [`PeerSet`](super::set::PeerSet).
//!
//! Candidate selection divides the set of all possible outbound peers into
//! disjoint subsets, using the [`PeerAddrState`](crate::PeerAddrState):
//!
//! 1. [`Responded`] peers, which we have had an outbound connection to.
//! 2. [`NeverAttemptedGossiped`] peers, which we learned about from other peers
//!    but have never connected to. This includes gossiped peers, DNS seeder peers,
//!    cached peers, canonical addresses from the [`Version`] messages of inbound
//!    and outbound connections, and remote IP addresses of inbound connections.
//! 3. [`Failed`] peers, which failed a connection attempt, or had an error
//!    during an outbound connection.
//! 4. [`AttemptPending`] peers, which we've recently queued for a connection.
//!
//! Never attempted peers are always available for connection.
//!
//! If a peer's attempted, responded, or failure time is recent
//! (within the liveness limit), we avoid reconnecting to it.
//! Otherwise, we assume that it has disconnected or hung,
//! and attempt reconnection.
//!
//! ```ascii,no_run
//!                         ┌──────────────────┐
//!                         │   Config / DNS   │
//!             ┌───────────│       Seed       │───────────┐
//!             │           │    Addresses     │           │
//!             │           └──────────────────┘           │
//!             │                    │ untrusted_last_seen │
//!             │                    │     is unknown      │
//!             ▼                    │                     ▼
//!    ┌──────────────────┐          │          ┌──────────────────┐
//!    │    Handshake     │          │          │     Peer Set     │
//!    │    Canonical     │──────────┼──────────│     Gossiped     │
//!    │    Addresses     │          │          │    Addresses     │
//!    └──────────────────┘          │          └──────────────────┘
//!     untrusted_last_seen          │                provides
//!         set to now               │           untrusted_last_seen
//!                                  ▼
//!                                  Λ   if attempted, responded, or failed:
//!                                 ╱ ╲         ignore gossiped info
//!                                ▕   ▏    otherwise, if never attempted:
//!                                 ╲ ╱    skip updates to existing fields
//!                                  V
//!  ┌───────────────────────────────┼───────────────────────────────┐
//!  │ AddressBook                   │                               │
//!  │ disjoint `PeerAddrState`s     ▼                               │
//!  │ ┌─────────────┐  ┌─────────────────────────┐  ┌─────────────┐ │
//!  │ │ `Responded` │  │`NeverAttemptedGossiped` │  │  `Failed`   │ │
//! ┌┼▶│    Peers    │  │          Peers          │  │   Peers     │◀┼┐
//! ││ └─────────────┘  └─────────────────────────┘  └─────────────┘ ││
//! ││        │                      │                      │        ││
//! ││ #1 oldest_first        #2 newest_first        #3 oldest_first ││
//! ││        ├──────────────────────┴──────────────────────┘        ││
//! ││        ▼                                                      ││
//! ││        Λ                                                      ││
//! ││       ╱ ╲              filter by                              ││
//! ││      ▕   ▏   is_ready_for_connection_attempt                  ││
//! ││       ╲ ╱     to remove recent `Responded`,                   ││
//! ││        V  `AttemptPending`, and `Failed` peers                ││
//! ││        │                                                      ││
//! ││        │    try outbound connection,                          ││
//! ││        ▼  update last_attempt to now()                        ││
//! ││┌────────────────┐                                             ││
//! │││`AttemptPending`│                                             ││
//! │││     Peers      │                                             ││
//! ││└────────────────┘                                             ││
//! │└────────┼──────────────────────────────────────────────────────┘│
//! │         ▼                                                       │
//! │         Λ                                                       │
//! │        ╱ ╲                                                      │
//! │       ▕   ▏─────────────────────────────────────────────────────┘
//! │        ╲ ╱   connection failed, update last_failure to now()
//! │         V
//! │         │
//! │         │ connection succeeded
//! │         ▼
//! │  ┌────────────┐
//! │  │    send    │
//! │  │peer::Client│
//! │  │to Discover │
//! │  └────────────┘
//! │         │
//! │         ▼
//! │┌───────────────────────────────────────┐
//! ││ when connection succeeds, and every   │
//! ││  time we receive a peer heartbeat:    │
//! └│  * update state to `Responded`        │
//!  │  * update last_response to now()      │
//!  └───────────────────────────────────────┘
//! ```
//!
//! [`Responded`]: crate::PeerAddrState::Responded
//! [`Version`]: crate::protocol::external::types::Version
//! [`NeverAttemptedGossiped`]: crate::PeerAddrState::NeverAttemptedGossiped
//! [`Failed`]: crate::PeerAddrState::Failed
//! [`AttemptPending`]: crate::PeerAddrState::AttemptPending

use std::{
    cmp::min,
    task::{Context, Poll},
};

use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use tokio::time::timeout;
use tower::{Service, ServiceExt};

use zebra_chain::serialization::DateTime32;

use crate::{
    address_book_updater::{AddressBookRequest, AddressBookResponse, AddressBookService},
    constants,
    meta_addr::MetaAddrChange,
    peer_set::set::MorePeers,
    types::MetaAddr,
    BoxError, Request, Response,
};

mod rate_limit;

pub(crate) use rate_limit::{RateLimitOnYield, SkipRateLimit};

#[cfg(test)]
mod tests;

/// The service that chooses the next reconnection candidate.
///
/// # Security
///
/// Rate-limited so new outbound connections are started at least
/// [`MIN_OUTBOUND_PEER_CONNECTION_INTERVAL`][constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL]
/// apart. The rate limit is only charged when a candidate is actually
/// yielded: an empty address book returns `None` immediately.
///
/// Clones share the same rate limit.
pub(crate) type NextPeerService = RateLimitOnYield<AddressBookService, AddressBookResponse>;

/// The service that crawls the network for more peer addresses.
///
/// # Security
///
/// Rate-limited to at most one crawl per
/// [`MIN_PEER_GET_ADDR_INTERVAL`][constants::MIN_PEER_GET_ADDR_INTERVAL],
/// to prevent sending a burst of repeated requests for new peer addresses.
/// While the rate limit applies, crawls are skipped.
///
/// Clones share the same rate limit.
pub(crate) type CrawlService<S> = SkipRateLimit<CrawlFanout<S>>;

/// Builds the candidate selection services used by the crawler:
/// a [`NextPeerService`] and a [`CrawlService`].
///
/// Uses `address_book_service` to choose candidates and store crawled
/// addresses, and `peer_service` to crawl the network for more addresses.
pub(crate) fn crawler_services<S>(
    address_book_service: AddressBookService,
    peer_service: S,
) -> (NextPeerService, CrawlService<S>)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let next_peer_service = RateLimitOnYield::new(
        address_book_service.clone(),
        constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL,
        |response| matches!(response, AddressBookResponse::NextReconnectPeer(Some(_))),
    );

    let crawl_service = SkipRateLimit::new(
        CrawlFanout {
            peer_service,
            address_book_service,
        },
        constants::MIN_PEER_GET_ADDR_INTERVAL,
    );

    (next_peer_service, crawl_service)
}

/// Crawl the network for more peer addresses, limiting the fanout to
/// `fanout_limit` if it is `Some`.
///
/// - Ask a few live [`Responded`] peers to send us more peers.
/// - Process all completed peer responses, adding new peers in the
///   [`NeverAttemptedGossiped`] state.
///
/// Returns `Some(MorePeers)` if the crawl was successful and the crawler
/// should ask for more peers. Returns `None` if there are no new peers.
///
/// ## Correctness
///
/// Pass the initial peer set size as `fanout_limit` during initialization,
/// so that Zebra does not send duplicate requests to the same peer.
///
/// The crawler exits when a crawl returns an error, so errors are only
/// returned on permanent failures.
///
/// The handshaker sets up the peer message receiver so it also sends a
/// [`Responded`] peer address update.
///
/// [`next_reconnect_peer`] puts peers into the [`AttemptPending`] state.
///
/// ## Security
///
/// This call is rate-limited to prevent sending a burst of repeated requests for new peer
/// addresses. Each call will only crawl the network if more time
/// than [`MIN_PEER_GET_ADDR_INTERVAL`][constants::MIN_PEER_GET_ADDR_INTERVAL] has passed since
/// the last crawl. Otherwise, the crawl is skipped.
///
/// [`Responded`]: crate::PeerAddrState::Responded
/// [`NeverAttemptedGossiped`]: crate::PeerAddrState::NeverAttemptedGossiped
/// [`Failed`]: crate::PeerAddrState::Failed
/// [`AttemptPending`]: crate::PeerAddrState::AttemptPending
pub(crate) async fn crawl_once<S>(
    crawl_service: &mut CrawlService<S>,
    fanout_limit: Option<usize>,
) -> Result<Option<MorePeers>, BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    // SECURITY
    //
    // Rate limit sending `GetAddr` messages to peers:
    // while the crawl service is rate-limited, calls are skipped,
    // and return `None` without contacting any peers.
    crawl_service.ready().await?.call(fanout_limit).await
}

/// Returns the next candidate for a connection attempt, if any are available.
///
/// Returns peers in reconnection order, based on
/// [`AddressBook::reconnection_peers`](crate::AddressBook::reconnection_peers).
///
/// Skips peers that have recently been active, attempted, or failed.
///
/// ## Correctness
///
/// `AttemptPending` peers will become [`Responded`] if they respond, or
/// become `Failed` if they time out or provide a bad response.
///
/// Live [`Responded`] peers will stay live if they keep responding, or
/// become a reconnection candidate if they stop responding.
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by making sure that
/// new peer connections are initiated at least
/// [`MIN_OUTBOUND_PEER_CONNECTION_INTERVAL`][constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL]
/// apart. If a peer was recently provided, then this future will sleep
/// until the rate-limit has passed.
///
/// [`Responded`]: crate::PeerAddrState::Responded
pub(crate) async fn next_reconnect_peer(
    next_peer_service: &mut NextPeerService,
) -> Option<MetaAddr> {
    // Atomically choose the next peer and mark it as `AttemptPending`,
    // in a single address book updater request.
    //
    // Security: new outbound peer connections are rate-limited by the
    // [`RateLimitOnYield`] middleware, which only sleeps before yielding
    // an address: when there is no peer, `None` is returned immediately.
    let response = match next_peer_service.ready().await {
        Ok(next_peer_service) => {
            next_peer_service
                .call(AddressBookRequest::NextReconnectPeer)
                .await
        }
        Err(error) => Err(error),
    };

    match response {
        Ok(AddressBookResponse::NextReconnectPeer(next_peer)) => next_peer,
        Ok(_) => unreachable!("NextReconnectPeer requests always return NextReconnectPeer"),
        Err(error) => {
            debug!(
                ?error,
                "error requesting next reconnection peer, is Zebra shutting down?"
            );
            None
        }
    }
}

/// Returns the number of candidate peers that are currently ready for a
/// connection attempt, as selected by [`next_reconnect_peer`].
///
/// The returned count is a snapshot: candidates can become ready or be
/// attempted by other tasks immediately afterwards.
pub(crate) async fn ready_peer_count(address_book_service: &AddressBookService) -> usize {
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::ReadyPeerCount)
        .boxed()
        .await;

    match response {
        Ok(AddressBookResponse::ReadyPeerCount(ready_peer_count)) => ready_peer_count,
        Ok(_) => unreachable!("ReadyPeerCount requests always return ReadyPeerCount"),
        Err(error) => {
            debug!(
                ?error,
                "error requesting ready peer count, is Zebra shutting down?"
            );
            0
        }
    }
}

/// A service that crawls the network for more peer addresses.
///
/// It fans out `GetPeers` requests to the peer set, validates the responses,
/// and sends the validated addresses to the address book updater.
///
/// The request is an optional fanout limit, and the response is
/// `Some(MorePeers)` if the crawl received any new peers.
#[derive(Clone)]
pub(crate) struct CrawlFanout<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    /// The peer set used to crawl the network for peers.
    peer_service: S,

    /// The buffered service handle to the address book updater task.
    address_book_service: AddressBookService,
}

impl<S> Service<Option<usize>> for CrawlFanout<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Option<MorePeers>;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Option<MorePeers>, BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Readiness of the peer service is checked before each fanout
        // request inside `call`.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, fanout_limit: Option<usize>) -> Self::Future {
        let peer_service = self.peer_service.clone();
        let address_book_service = self.address_book_service.clone();

        async move {
            // CORRECTNESS
            //
            // Use a timeout to avoid deadlocks when there are no connected
            // peers, and:
            // - we're waiting on a handshake to complete so there are peers, or
            // - another task that handles or adds peers is waiting on this task
            //   to complete.
            match timeout(
                constants::PEER_GET_ADDR_TIMEOUT,
                crawl_fanout(peer_service, address_book_service, fanout_limit),
            )
            .await
            {
                Ok(fanout_result) => fanout_result,
                Err(_elapsed) => {
                    // crawls must only return an error for permanent failures
                    info!("timeout waiting for peer service readiness or peer responses");
                    Ok(None)
                }
            }
        }
        .boxed()
    }
}

/// Crawl the network for more peer addresses, limiting the fanout to
/// `fanout_limit`.
///
/// Opportunistically crawl the network on every call to ensure
/// we're actively fetching peers. Continue independently of whether we
/// actually receive any peers, but always ask the network for more.
///
/// Because requests are load-balanced across existing peers, we can make
/// multiple requests concurrently, which will be randomly assigned to
/// existing peers, but we don't make too many because crawls may be
/// started while the peer set is already loaded.
///
/// # Correctness
///
/// This function does not have a timeout.
/// Use the [`CrawlFanout`] service instead.
async fn crawl_fanout<S>(
    mut peer_service: S,
    address_book_service: AddressBookService,
    fanout_limit: Option<usize>,
) -> Result<Option<MorePeers>, BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let fanout_limit = fanout_limit
        .map(|fanout_limit| min(fanout_limit, constants::GET_ADDR_FANOUT))
        .unwrap_or(constants::GET_ADDR_FANOUT);
    debug!(?fanout_limit, "sending GetPeers requests");

    let mut responses = FuturesUnordered::new();
    let mut more_peers = None;

    // Launch requests
    for attempt in 0..fanout_limit {
        if attempt > 0 {
            // Let other tasks run, so we're more likely to choose a different peer.
            //
            // TODO: move fanouts into the PeerSet, so we always choose different peers (#2214)
            tokio::task::yield_now().await;
        }

        let peer_service = peer_service.ready().await?;
        responses.push(peer_service.call(Request::Peers));
    }

    let mut address_book_updates = FuturesUnordered::new();

    // Process responses
    while let Some(rsp) = responses.next().await {
        match rsp {
            Ok(Response::Peers(addrs)) => {
                trace!(
                    addr_count = ?addrs.len(),
                    ?addrs,
                    "got response to GetPeers"
                );
                let addrs = validate_addrs(addrs, DateTime32::now());
                address_book_updates.push(send_addrs(address_book_service.clone(), addrs));
                more_peers = Some(MorePeers);
            }
            Err(e) => {
                // since we do a fanout, and new updates are triggered by
                // each demand, we can ignore errors in individual responses
                trace!(?e, "got error in GetPeers request");
            }
            Ok(_) => unreachable!("Peers requests always return Peers responses"),
        }
    }

    // Wait until all the address book updates have finished
    while let Some(()) = address_book_updates.next().await {}

    Ok(more_peers)
}

/// Add new `addrs` to the address book.
async fn send_addrs(
    address_book_service: AddressBookService,
    addrs: impl IntoIterator<Item = MetaAddr>,
) {
    // # Security
    //
    // New gossiped peers are rate-limited because:
    // - Zebra initiates requests for new gossiped peers
    // - the fanout is limited
    // - the number of addresses per peer is limited
    let addrs: Vec<MetaAddrChange> = addrs
        .into_iter()
        .map(MetaAddr::new_gossiped_change)
        .map(|maybe_addr| maybe_addr.expect("Received gossiped peers always have services set"))
        .collect();

    debug!(count = ?addrs.len(), "sending gossiped addresses to the address book");

    // Don't bother making a request if there are no addresses left.
    if addrs.is_empty() {
        return;
    }

    // Extend handles duplicate addresses internally.
    //
    // Correctness: box the request future, so its captured types don't
    // leak into generic callers (rustc's async Send inference struggles
    // with boxed trait objects inside generic spawned tasks).
    let result = address_book_service
        .oneshot(AddressBookRequest::ExtendGossiped(addrs))
        .boxed()
        .await;

    if let Err(error) = result {
        debug!(
            ?error,
            "error sending gossiped addresses to the address book, is Zebra shutting down?"
        );
    }
}

/// Check new `addrs` before adding them to the address book.
///
/// `last_seen_limit` is the maximum permitted last seen time, typically
/// [`Utc::now`](chrono::Utc::now).
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
    // - Zebra should count back 3 weeks from the newest peer timestamp sent
    //   by the other peer, to compensate for clock skew

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
