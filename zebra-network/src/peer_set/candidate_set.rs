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
//
// TODO:
//   * show all possible transitions between Attempt/Responded/Failed,
//     except Failed -> Responded is invalid, must go through Attempt

use futures::FutureExt;
use tower::{Service, ServiceExt};

use crate::{
    address_book_updater::{AddressBookRequest, AddressBookResponse, AddressBookService},
    constants,
    types::MetaAddr,
    BoxError, Request, Response,
};

mod crawl;
mod rate_limit;

use crawl::CrawlFanout;
pub(crate) use crawl::{crawl_once, CrawlService};
pub(crate) use rate_limit::{RateLimitBySkipping, RateLimitOnYield};

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

    let crawl_service = CrawlFanout::service(peer_service, address_book_service);

    (next_peer_service, crawl_service)
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
