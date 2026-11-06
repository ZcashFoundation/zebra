//! Serving `get-hashes` requests (draft, "`get-hashes`"; design §5).
//!
//! A request asks for the best-chain sync metadata of up to
//! [`MAX_GET_HASHES_COUNT`](crate::protocol::v2::constants::MAX_GET_HASHES_COUNT)
//! heights, which the inbound service reads with one range scan; the response
//! is at most about 2 MB. The exchange stays inside the request stream's
//! whole-exchange deadline, like the other request types, and its response
//! write has a shorter deadline of its own, [`HASHES_WRITE_TIMEOUT`]. It is
//! bounded by its own permits rather than the peer's
//! [`INBOUND_REQUEST_CONCURRENCY`](super::INBOUND_REQUEST_CONCURRENCY) ones,
//! so bulk sync traffic never queues behind, or starves, the peer's
//! `get-headers` and `get-blocks` requests.
//!
//! Ported from #11349's `GetHashes` serving arm
//! (`zebra-network/src/peer/v2/connection.rs` on
//! `p2p-v2-6-known-hash-formats`), without the stride.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tower::Service;

use crate::{
    protocol::{
        internal::{Request, Response},
        v2::{request::GetHashes, response::hashes::encode_hashes_response, types::ErrorCode},
    },
    BoxError,
};

use super::{call_service, Inbound, Outcome, ResponseSend};

/// How many of one peer's `get-hashes` requests are served at once; the rest
/// are refused rather than queued.
pub(in crate::peer::v2::connection) const HASHES_STREAMS_PER_PEER: usize = 2;

/// How many `get-hashes` requests this node serves at once, over all its
/// peers; the rest are refused rather than queued.
///
/// # Security
///
/// Each served request reads up to `MAX_GET_HASHES_COUNT` rows of the sync
/// metadata and the hash index, and holds a response of about 2 MB until it
/// is written or [`HASHES_WRITE_TIMEOUT`] passes, so this bounds the disk
/// reads and the memory of `get-hashes` serving, whatever the number of
/// peers.
pub(in crate::peer::v2::connection) const HASHES_STREAMS_GLOBAL: usize = 8;

/// How long the response write of a `get-hashes` request may take before the
/// stream is abandoned, which releases its permits.
///
/// A full response of about 2 MB must be read at about 400 KB/s to finish in
/// time. A requester on a slower link asks for fewer heights at once:
/// 10,000 heights, about 400 KB, need about 80 KB/s.
///
/// # Security
///
/// The write holds one of the node's [`HASHES_STREAMS_GLOBAL`] permits. With
/// only the request deadline, `REQUEST_TIMEOUT`, a few peers that request
/// full responses and never read them hold every global permit for that
/// long. This deadline is well below `REQUEST_TIMEOUT`, so a stalled reader
/// loses its permit after a few seconds, and its connection's `get-hashes`
/// requests are then refused for [`HASHES_STALL_BACKOFF`], before they can
/// take a global permit again.
///
/// So a peer that stalls its reads holds its connection's permits for at
/// most this deadline out of every [`HASHES_STALL_BACKOFF`]. A new connection
/// starts without a backoff, but the listener accepts only
/// `max_connections_per_ip` inbound connection attempts per IPv4 address or
/// IPv6 `/64` in every `MIN_PEER_RECONNECTION_DELAY` (about two minutes), so
/// reconnecting adds at most one more stalled response per address in that
/// time. One address then holds its connection's 2 permits for at most about
/// an eighth of the time, so keeping all [`HASHES_STREAMS_GLOBAL`] permits
/// held takes about 30 addresses that stall continuously, instead of 4.
pub(in crate::peer::v2::connection) const HASHES_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a connection's `get-hashes` requests are refused after one of its
/// responses was abandoned at [`HASHES_WRITE_TIMEOUT`].
///
/// An honest requester whose response stalled asks another peer, or asks
/// again later for fewer heights, so it loses little. A peer that stalls on
/// purpose holds the node's permits for at most [`HASHES_WRITE_TIMEOUT`] in
/// every backoff, instead of all the time.
pub(in crate::peer::v2::connection) const HASHES_STALL_BACKOFF: Duration = Duration::from_secs(60);

/// When a connection's `get-hashes` requests are served again after one of
/// its responses stalled. Shared by the connection's request streams, without
/// a lock.
#[derive(Debug)]
pub(in crate::peer::v2::connection) struct StallBackoff {
    /// The time the backoff deadline is measured from.
    origin: Instant,
    /// The end of the backoff, in milliseconds after `origin`; zero before the
    /// first stall.
    until: AtomicU64,
}

impl StallBackoff {
    /// A connection's backoff, which refuses nothing until a response stalls.
    pub(in crate::peer::v2::connection) fn new() -> Self {
        StallBackoff {
            origin: Instant::now(),
            until: AtomicU64::new(0),
        }
    }

    /// Whether the connection's `get-hashes` requests are refused at `now`.
    fn backing_off(&self, now: Instant) -> bool {
        self.millis(now) < self.until.load(Ordering::Relaxed)
    }

    /// Records that a response stalled at `now`: refuses the connection's
    /// requests for [`HASHES_STALL_BACKOFF`].
    fn stalled(&self, now: Instant) {
        // The backoff is 60 s, so its milliseconds fit in a `u64`.
        let backoff = HASHES_STALL_BACKOFF.as_millis() as u64;
        self.until
            .fetch_max(self.millis(now).saturating_add(backoff), Ordering::Relaxed);
    }

    /// The milliseconds from `origin` to `now`.
    fn millis(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.origin).as_millis()).unwrap_or(u64::MAX)
    }
}
/// Returns the node-wide `get-hashes` permits, [`HASHES_STREAMS_GLOBAL`] of
/// them, to be shared by every version 2 connection.
pub(crate) fn global_hashes_permits() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(HASHES_STREAMS_GLOBAL))
}

/// The permits one served `get-hashes` request holds until its exchange ends.
struct Permits {
    _peer: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

/// Takes a permit from the peer's `peer_permits` and from the node's
/// `global_permits`, or refuses the stream with `REFUSED` if either is exhausted, or
/// if the peer's `backoff` after a stalled response has not ended.
///
/// # Security
///
/// Excess requests are refused at once, never queued: a waiting request would
/// hold its stream, and the requester can ask another peer. A refusal for a
/// busy node is not an overload: the overload policy that drops connections
/// is for requests the inbound service could not take.
fn try_acquire(
    peer_permits: &Arc<Semaphore>,
    backoff: &StallBackoff,
    global_permits: &Arc<Semaphore>,
) -> Result<Permits, Outcome> {
    try_acquire_at(peer_permits, backoff, global_permits, Instant::now())
        .ok_or(Outcome::Refuse(ErrorCode::Refused))
}

/// [`try_acquire`] at `now`.
fn try_acquire_at(
    peer_permits: &Arc<Semaphore>,
    backoff: &StallBackoff,
    global_permits: &Arc<Semaphore>,
    now: Instant,
) -> Option<Permits> {
    if backoff.backing_off(now) {
        return None;
    }
    let peer = peer_permits.clone().try_acquire_owned().ok()?;
    let global = global_permits.clone().try_acquire_owned().ok()?;
    Some(Permits {
        _peer: peer,
        _global: global,
    })
}

/// Serves a `get-hashes` request: the inbound service's entries, as one
/// response body, while holding a permit from the peer's `peer_permits` and
/// one of the node's `global_permits`. A response whose write is abandoned at
/// [`HASHES_WRITE_TIMEOUT`] starts the peer's `backoff`.
///
/// An empty answer is a response with no entries (`n = 0`), not a refusal:
/// the requester learns that this node has nothing to serve at
/// `start_height`.
pub(super) async fn serve_hashes<S>(
    response: &mut ResponseSend,
    request: GetHashes,
    peer_permits: &Arc<Semaphore>,
    backoff: &StallBackoff,
    global_permits: &Arc<Semaphore>,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    // Taken after the request is read and decoded: the transport's stream limit bounds requests
    // that are still arriving.
    let _permits = try_acquire(peer_permits, backoff, global_permits)?;

    let answer = call_service(
        service,
        Request::SyncHashes {
            start: request.start_height,
            count: request.count,
        },
    )
    .await?;
    let Response::SyncHashes(mut entries) = answer else {
        tracing::info!(%answer, "inbound service answered get-hashes with the wrong response");
        return Err(Outcome::Refuse(ErrorCode::Refused));
    };

    // A response with more entries than the request's count does not conform to the request, and
    // the requester closes the connection for it, so an inbound service that answers with more
    // only has the requested prefix sent.
    //
    // The count is a `u32`, which fits in a `usize` on every supported target.
    entries.truncate(request.count as usize);

    let body = encode_hashes_response(&entries)?;
    match write_within(HASHES_WRITE_TIMEOUT, response.write(&body)).await {
        Some(written) => written,
        // The permits are released when this returns, and the stream is reset like a request at
        // its deadline.
        None => {
            tracing::debug!(
                backoff = ?HASHES_STALL_BACKOFF,
                "get-hashes response write timed out, abandoning the stream \
                 and refusing the peer's get-hashes requests for a while"
            );
            backoff.stalled(Instant::now());
            Err(Outcome::Refuse(response.abandon_code()))
        }
    }
}

/// Runs `write`, or returns `None` if it does not finish within `deadline`.
async fn write_within<F: Future>(deadline: Duration, write: F) -> Option<F::Output> {
    tokio::time::timeout(deadline, write).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`try_acquire_at`] now, for a peer that has never stalled.
    fn acquire(peer: &Arc<Semaphore>, global: &Arc<Semaphore>) -> Option<Permits> {
        try_acquire_at(peer, &StallBackoff::new(), global, Instant::now())
    }

    #[test]
    fn peer_permits_refuse_excess_requests() {
        let _init_guard = zebra_test::init();
        let global = &global_hashes_permits();
        let peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));

        let held: Vec<_> = (0..HASHES_STREAMS_PER_PEER)
            .map(|_| acquire(&peer, global).expect("the peer has permits left"))
            .collect();
        assert!(
            acquire(&peer, global).is_none(),
            "a third concurrent request from one peer is refused"
        );
        assert_eq!(
            global.available_permits(),
            HASHES_STREAMS_GLOBAL - HASHES_STREAMS_PER_PEER,
            "a refused request holds no global permit"
        );

        // Another peer is not limited by this one's requests.
        let other_peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));
        assert!(acquire(&other_peer, global).is_some());

        // Finished requests return their permits.
        drop(held);
        assert!(acquire(&peer, global).is_some());
        assert_eq!(peer.available_permits(), HASHES_STREAMS_PER_PEER);
        assert_eq!(global.available_permits(), HASHES_STREAMS_GLOBAL);
    }

    #[test]
    fn global_permits_refuse_excess_requests() {
        let _init_guard = zebra_test::init();
        let global = &global_hashes_permits();
        let peers: Vec<_> = (0..HASHES_STREAMS_GLOBAL)
            .map(|_| Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER)))
            .collect();

        // One request from each of `HASHES_STREAMS_GLOBAL` peers takes every global permit.
        let held: Vec<_> = peers
            .iter()
            .map(|peer| acquire(peer, global).expect("global permits are left"))
            .collect();

        // Every peer still has a permit of its own, but the node is busy.
        let fresh_peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));
        assert!(acquire(&fresh_peer, global).is_none());
        assert!(acquire(&peers[0], global).is_none());
        assert_eq!(
            fresh_peer.available_permits(),
            HASHES_STREAMS_PER_PEER,
            "a refused request returns its peer permit"
        );

        drop(held);
        assert!(acquire(&fresh_peer, global).is_some());
    }

    /// Peers that never read their responses cannot keep another peer from
    /// being served for longer than the write deadline.
    #[tokio::test]
    async fn stalled_readers_release_their_permits_at_the_write_deadline() {
        let _init_guard = zebra_test::init();
        let global = &global_hashes_permits();
        let deadline = Duration::from_millis(100);

        // Four peers, each with every stream it may open, take all the global permits and stall
        // their writes.
        let stalled_peers = HASHES_STREAMS_GLOBAL / HASHES_STREAMS_PER_PEER;
        assert_eq!(stalled_peers, 4);
        let mut stalled = Vec::new();
        for _ in 0..stalled_peers {
            let peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));
            for _ in 0..HASHES_STREAMS_PER_PEER {
                let permits = acquire(&peer, global).expect("global permits are left");
                stalled.push(tokio::spawn(async move {
                    let written =
                        write_within(deadline, std::future::pending::<Result<(), Outcome>>()).await;
                    drop(permits);
                    written.is_none()
                }));
            }
        }

        // A fifth peer is refused while the writes stall.
        let fifth_peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));
        assert!(acquire(&fifth_peer, global).is_none());

        // The stalled writes are abandoned at the deadline, and the fifth peer is served.
        for task in stalled {
            assert!(
                task.await.expect("the task completes"),
                "the write timed out"
            );
        }
        assert!(acquire(&fifth_peer, global).is_some());
    }

    /// Peers that stall their reads and re-open their streams as soon as the
    /// write deadline frees their permits are refused for the backoff, so
    /// other peers are served in the meantime.
    #[tokio::test(start_paused = true)]
    async fn stalled_readers_that_reopen_are_refused_during_the_backoff() {
        let _init_guard = zebra_test::init();
        let global = &global_hashes_permits();

        let stalled_peers: Vec<_> = (0..HASHES_STREAMS_GLOBAL / HASHES_STREAMS_PER_PEER)
            .map(|_| {
                (
                    Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER)),
                    StallBackoff::new(),
                )
            })
            .collect();

        // The stalled peers take every global permit, and their writes time out.
        let mut held = Vec::new();
        for (peer, backoff) in &stalled_peers {
            for _ in 0..HASHES_STREAMS_PER_PEER {
                held.push(
                    try_acquire_at(peer, backoff, global, Instant::now())
                        .expect("global permits are left"),
                );
            }
        }
        let fifth_peer = Arc::new(Semaphore::new(HASHES_STREAMS_PER_PEER));
        let fifth_backoff = StallBackoff::new();
        assert!(try_acquire_at(&fifth_peer, &fifth_backoff, global, Instant::now()).is_none());

        tokio::time::advance(HASHES_WRITE_TIMEOUT).await;
        for (_, backoff) in &stalled_peers {
            backoff.stalled(Instant::now());
        }
        drop(held);

        // The stalled peers re-open their streams at once, and are refused.
        for (peer, backoff) in &stalled_peers {
            assert!(try_acquire_at(peer, backoff, global, Instant::now()).is_none());
        }
        assert_eq!(global.available_permits(), HASHES_STREAMS_GLOBAL);

        // Other peers are served throughout the backoff.
        tokio::time::advance(HASHES_STALL_BACKOFF - Duration::from_millis(1)).await;
        for (peer, backoff) in &stalled_peers {
            assert!(try_acquire_at(peer, backoff, global, Instant::now()).is_none());
        }
        let served = try_acquire_at(&fifth_peer, &fifth_backoff, global, Instant::now());
        assert!(served.is_some(), "a peer that never stalled is served");
        drop(served);

        // After the backoff, the stalled peers are served again.
        tokio::time::advance(Duration::from_millis(1)).await;
        let (peer, backoff) = &stalled_peers[0];
        assert!(try_acquire_at(peer, backoff, global, Instant::now()).is_some());
    }

    #[test]
    fn the_write_deadline_is_below_the_request_deadline() {
        assert!(HASHES_WRITE_TIMEOUT < crate::constants::REQUEST_TIMEOUT / 2);
    }

    #[test]
    fn the_node_wide_permits_are_bounded() {
        let _init_guard = zebra_test::init();

        assert_eq!(
            global_hashes_permits().available_permits(),
            HASHES_STREAMS_GLOBAL
        );
    }
}
