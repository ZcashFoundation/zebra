//! Crawling the network for more peer addresses.
//!
//! The crawler asks live [`Responded`] peers for more peers, validates the
//! responses, and sends the new addresses to the peer book actor. Candidate
//! selection for outbound connections lives in the
//! [parent module](super).
//!
//! [`Responded`]: crate::PeerAddrState::Responded

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

use crate::{
    constants,
    peer_book::{PeerBookHandle, PeerBookRequest},
    peer_set::set::MorePeers,
    types::MetaAddr,
    BoxError, Request, Response,
};

use super::RateLimitBySkipping;

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
pub(crate) type CrawlService<S> = RateLimitBySkipping<CrawlFanout<S>>;

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
/// [`next_reconnect_peer`]: super::next_reconnect_peer
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

/// A service that crawls the network for more peer addresses.
///
/// It fans out `GetPeers` requests to the peer set, and sends the returned
/// addresses to the peer book actor, which validates them before storing.
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

    /// A handle to the peer book actor, which validates and stores the
    /// crawled addresses.
    peer_book_handle: PeerBookHandle,
}

impl<S> CrawlFanout<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    /// Returns a rate-limited [`CrawlService`] that crawls using `peer_service`,
    /// and stores the addresses it finds using `peer_book_handle`.
    pub(super) fn service(peer_service: S, peer_book_handle: PeerBookHandle) -> CrawlService<S> {
        RateLimitBySkipping::new(
            Self {
                peer_service,
                peer_book_handle,
            },
            constants::MIN_PEER_GET_ADDR_INTERVAL,
        )
    }
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
        let peer_book_handle = self.peer_book_handle.clone();

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
                crawl_fanout(peer_service, peer_book_handle, fanout_limit),
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
    peer_book_handle: PeerBookHandle,
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

    let mut peer_book_updates = FuturesUnordered::new();

    // Process responses
    while let Some(rsp) = responses.next().await {
        match rsp {
            Ok(Response::Peers(addrs)) => {
                trace!(
                    addr_count = ?addrs.len(),
                    ?addrs,
                    "got response to GetPeers"
                );
                peer_book_updates.push(send_addrs(peer_book_handle.clone(), addrs));
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

    // Wait until all the peer book updates have finished
    while let Some(()) = peer_book_updates.next().await {}

    Ok(more_peers)
}

/// Add new gossiped `addrs` to the peer book.
async fn send_addrs(peer_book_handle: PeerBookHandle, addrs: Vec<MetaAddr>) {
    // # Security
    //
    // New gossiped peers are rate-limited because:
    // - Zebra initiates requests for new gossiped peers
    // - the fanout is limited
    // - the number of addresses per peer is limited
    //
    // The peer book actor validates the addresses before storing them.
    debug!(count = ?addrs.len(), "sending gossiped addresses to the peer book");

    // Don't bother making a request if there are no addresses left.
    if addrs.is_empty() {
        return;
    }

    // The actor handles duplicate addresses internally.
    //
    // Correctness: box the request future, so its captured types don't
    // leak into generic callers (rustc's async Send inference struggles
    // with boxed trait objects inside generic spawned tasks).
    let result = peer_book_handle
        .oneshot(PeerBookRequest::GossipedAddrs { addrs })
        .boxed()
        .await;

    if let Err(error) = result {
        debug!(
            ?error,
            "error sending gossiped addresses to the peer book, is Zebra shutting down?"
        );
    }
}
