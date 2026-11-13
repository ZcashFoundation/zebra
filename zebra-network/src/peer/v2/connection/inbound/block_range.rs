//! Serving `get-block-range` requests (draft, "`get-block-range`").
//!
//! A `get-block-range` response is a `result` byte, then the ancestor chain of
//! the request's `final_hash`, highest block first, until `count` blocks or
//! `max_bytes` bytes of them (the first block exempt) have been sent. The
//! response is read from the inbound service in chunks of at most
//! [`RANGE_READ_CHUNK_BLOCKS`] blocks and [`RANGE_READ_CHUNK`] bytes, each
//! continuing from the last block's parent, and each block is written as it
//! is read.
//!
//! A range can be up to 64 MiB, far more than a peer can be expected to read
//! within one request deadline, so a range response is not bounded by the
//! whole-exchange deadline of the other request types. Instead:
//! - each inbound service call must answer within [`REQUEST_TIMEOUT`], which
//!   bounds this node's own reads;
//! - the peer must take the response at [`RANGE_MIN_SERVE_RATE`] or faster,
//!   measured from the moment its request is complete and over the time the
//!   response's writes wait for it, with [`RANGE_FIRST_WRITE_TIMEOUT`] to
//!   start and at most [`RANGE_PROGRESS_WINDOW`] of slack later (see
//!   [`Pace`]). A write is abandoned at the moment the peer falls behind, so
//!   a reader that grants no flow-control credit holds its permits for
//!   seconds;
//! - the whole stream must be done within [`RANGE_STREAM_TIMEOUT`] of being
//!   accepted, so no stream holds a permit indefinitely.
//!
//! A response that misses any of them is abandoned like any other: reset
//! with `REFUSED` while nothing is written, and `CANCELLED` once it is (see
//! the parent module).
//!
//! # Security
//!
//! A request costs the peer 41 bytes and can ask for 64 MiB, so range
//! streams are bounded on their own, separately from the
//! [`INBOUND_REQUEST_CONCURRENCY`](super::INBOUND_REQUEST_CONCURRENCY)
//! permits of the other request types, which range streams would otherwise
//! starve: at most [`RANGE_STREAMS_PER_PEER`] per peer and
//! [`RANGE_STREAMS_GLOBAL`] per node. Excess streams are refused at once with
//! `BUSY`, never queued, so the requester asks another peer and may ask this
//! node again later. `REFUSED` is kept for a node that does not serve ranges. A stream
//! takes its peer permit when its type byte is read, and its node permit
//! only once its request is decoded, so streams that never send a request
//! hold only their own peer's permits. Each stream
//! holds at most one chunk of blocks, so the memory and disk reads of range
//! serving are bounded whatever the number of peers.
//!
//! The node's permits are shared between network addresses: the connections
//! from one address hold at most [`RANGE_STREAMS_PER_ADDR`] of them, and
//! those from one address group at most [`RANGE_STREAMS_PER_GROUP`], so one
//! host or network cannot hold them all while other peers are answered with
//! `BUSY`.
//!
//! A response abandoned because its reader fell behind has held a node permit
//! for almost nothing, and without a cost the peer could open the next stream
//! at once and hold the permit again. So that peer's further range streams
//! are answered with `BUSY` for [`RANGE_STALL_BACKOFF`], doubling each time
//! it happens again, both on the connection and for its address (see
//! [`NodeRangeLimits`]), so a new connection does not clear it.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tower::Service;

use zebra_chain::{block, parameters::GENESIS_PREVIOUS_BLOCK_HASH, serialization::ZcashSerialize};

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::{
        external::connection_limit_key,
        internal::{Request, Response},
        v2::{
            request::GetBlockRange,
            response::block_range::{
                write_block_range_answer, write_block_range_element, BlockRangeAnswer,
            },
            types::{ErrorCode, StreamType},
            wire::WireError,
        },
    },
    v2_address_book::Group,
    BoxError,
};

use super::{call_service, Inbound, Outcome, ResponseSend};

/// How many `get-block-range` streams one peer may have served at once; the
/// rest are refused rather than queued.
///
/// The requester's own bound on its bulk streams to one peer is the same, so
/// a conforming requester is never refused for this limit.
pub(crate) const RANGE_STREAMS_PER_PEER: usize = 2;

/// How many `get-block-range` streams this node serves at once, over all its
/// peers; the rest are refused rather than queued.
///
/// # Security
///
/// Each served stream holds at most one chunk of blocks, at most
/// [`RANGE_READ_CHUNK`] of serialized blocks plus the chunk's first block,
/// and two buffers of up to one block's serialization each (the block being
/// written, and its response element). At the 2 MB block size limit that is
/// at most about 8 MB of serialized block data per stream, plus the
/// in-memory overhead of the chunk's deserialized blocks, which are dropped
/// as they are written. So this bounds the memory and the state reads of
/// range serving, whatever the number of peers.
pub(crate) const RANGE_STREAMS_GLOBAL: usize = 8;

/// How many of the node's `get-block-range` streams the connections from one
/// remote address may hold at once, keyed like connection limits
/// ([`connection_limit_key`]): as many as one peer, so that more connections
/// from an address get it no more of the node's permits.
pub(crate) const RANGE_STREAMS_PER_ADDR: usize = RANGE_STREAMS_PER_PEER;

/// How many of the node's `get-block-range` streams the connections from one
/// address group (an IPv4 /16 or IPv6 /32, see [`Group`]) may hold at once.
///
/// # Security
///
/// Half of [`RANGE_STREAMS_GLOBAL`], so that filling the node's permits
/// takes addresses in at least two groups, and peers in other groups are
/// still served while one group holds its whole share.
pub(crate) const RANGE_STREAMS_PER_GROUP: usize = RANGE_STREAMS_GLOBAL / 2;

/// The most serialized bytes of blocks one inbound service call reads for a
/// `get-block-range` response, the call's first block exempt.
pub(crate) const RANGE_READ_CHUNK: u32 = 2 * 1024 * 1024;

/// The most blocks one inbound service call reads for a `get-block-range`
/// response.
pub(crate) const RANGE_READ_CHUNK_BLOCKS: u32 = 64;

/// How long after its stream was accepted a `get-block-range` response must
/// be complete, after which it is abandoned.
///
/// A reader at [`RANGE_MIN_SERVE_RATE`] takes the draft's 64 MiB maximum in
/// 256 s, so the minimum rate, not this deadline, is what bounds a reader.
/// The deadline leaves the rest for this node's own reads, which are not
/// counted against the reader, and bounds a stream whatever happens.
pub(crate) const RANGE_STREAM_TIMEOUT: Duration = Duration::from_secs(300);

/// The lowest rate, in response bytes per second, at which a peer must take
/// a `get-block-range` response, below which the response is abandoned (see
/// [`Pace`]).
///
/// # Security
///
/// Without it, a reader could take each write just within a deadline and
/// hold a node permit for the whole [`RANGE_STREAM_TIMEOUT`] at almost no
/// cost: a few such peers would deny range serving to every other peer.
///
/// A reader exactly at the floor still holds a permit for as long as the
/// floor lets it, so the floor is high enough that it has to take a whole
/// 64 MiB response within the stream deadline: holding a permit for the
/// deadline costs the reader, and this node, the full response. At 64 KiB/s
/// it cost a quarter of one.
///
/// 256 KiB/s is also the default rate below which Zebra's own block-range
/// requester gives up on a peer (`sync.v2_min_peer_rate`), so a response
/// abandoned here is one such a requester would have abandoned anyway. The
/// cost is that a node whose uplink cannot give each stream it is serving
/// this rate abandons the slowest of them, which frees its share for the
/// others.
pub(crate) const RANGE_MIN_SERVE_RATE: u64 = 256 * 1024;

/// How long the peer has, from the moment its `get-block-range` request is
/// complete, to start taking the response: the time the first write may wait
/// for the peer beyond what its bytes take at [`RANGE_MIN_SERVE_RATE`].
///
/// # Security
///
/// A peer that sends a request and grants no stream flow-control credit
/// blocks the first write. This is how long it holds a node permit for the
/// 41 bytes of its request, so it is a few seconds, not a request deadline.
pub(crate) const RANGE_FIRST_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most slack a `get-block-range` reader that is ahead of
/// [`RANGE_MIN_SERVE_RATE`] keeps: how long it may then stop reading before
/// the response is abandoned. It rides out a short network stall without
/// letting a reader that started fast hold a permit while it reads nothing.
pub(crate) const RANGE_PROGRESS_WINDOW: Duration = Duration::from_secs(10);

/// How long a peer's `get-block-range` streams are answered with `BUSY`
/// after one of its responses was abandoned because it fell behind its
/// minimum progress. Each further abandoned response doubles it, up to
/// [`RANGE_STALL_BACKOFF_MAX`].
///
/// # Security
///
/// A stalled response holds a node permit for at least
/// [`RANGE_FIRST_WRITE_TIMEOUT`] at the cost of one request. With the
/// backoff, an address that stalls every response it is served holds a
/// permit for a small and shrinking share of the time, so holding the
/// node's permits takes many addresses, not a few.
pub(crate) const RANGE_STALL_BACKOFF: Duration = Duration::from_secs(60);

/// The longest [`RANGE_STALL_BACKOFF`] grows to, and how long after a
/// backoff ends the next abandoned response still doubles it.
pub(crate) const RANGE_STALL_BACKOFF_MAX: Duration = Duration::from_secs(16 * 60);

/// The most addresses whose stalled responses the node remembers.
///
/// # Security
///
/// The table is bounded so that peers cannot grow it: when it is full, the
/// entry whose backoff ends first is dropped for a new one. An entry is a
/// few dozen bytes.
pub(crate) const RANGE_STALL_ADDRS: usize = 4096;

/// Returns the node-wide `get-block-range` serving state, with
/// [`RANGE_STREAMS_GLOBAL`] stream permits, to be shared by every version 2
/// connection.
pub(crate) fn global_block_range_permits() -> Arc<NodeRangeLimits> {
    Arc::new(NodeRangeLimits {
        permits: Arc::new(Semaphore::new(RANGE_STREAMS_GLOBAL)),
        addrs: Mutex::new(AddrState::default()),
    })
}

/// The `get-block-range` serving state every version 2 connection of a node
/// shares: its stream permits, and what it remembers about remote addresses.
///
/// Addresses are keyed by [`connection_limit_key`], like connection limits
/// and bans, so an IPv6 host cannot get a fresh key from its own subnet.
#[derive(Debug)]
pub(crate) struct NodeRangeLimits {
    /// The node's permits, [`RANGE_STREAMS_GLOBAL`] of them.
    permits: Arc<Semaphore>,

    /// What the node remembers about remote addresses. The lock is only held
    /// for a table lookup or update, never across an await.
    addrs: Mutex<AddrState>,
}

/// What a node remembers about the remote addresses it serves ranges to.
#[derive(Debug, Default)]
struct AddrState {
    /// How many node permits the connections from each address hold.
    streams_by_addr: HashMap<IpAddr, usize>,

    /// How many node permits the connections from each address group hold.
    streams_by_group: HashMap<Group, usize>,

    /// The addresses whose responses were abandoned because they fell
    /// behind, at most [`RANGE_STALL_ADDRS`] of them.
    stalls: HashMap<IpAddr, Stall>,
}

impl NodeRangeLimits {
    /// How many of the node's stream permits are free.
    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }

    /// How many addresses' stalled responses are remembered.
    #[cfg(test)]
    pub(crate) fn remembered_stalls(&self) -> usize {
        self.lock().stalls.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AddrState> {
        self.addrs
            .lock()
            .expect("the lock is only held for map operations, which do not panic")
    }

    /// Claims a share of the node's permits for one stream of a peer at
    /// `remote`, or refuses the stream with `BUSY` if the address or its
    /// group already holds its whole share. Never waits.
    fn claim_addr(self: &Arc<Self>, remote: IpAddr) -> Result<AddrShare, Outcome> {
        let addr = connection_limit_key(remote);
        let group = Group::of(addr);
        let mut state = self.lock();
        if state.streams_by_addr.get(&addr).copied().unwrap_or(0) >= RANGE_STREAMS_PER_ADDR {
            return Err(busy("address limit"));
        }
        if state.streams_by_group.get(&group).copied().unwrap_or(0) >= RANGE_STREAMS_PER_GROUP {
            return Err(busy("address group limit"));
        }
        *state.streams_by_addr.entry(addr).or_default() += 1;
        *state.streams_by_group.entry(group).or_default() += 1;
        Ok(AddrShare {
            node: self.clone(),
            addr,
            group,
        })
    }

    /// Whether `addr`'s range streams are answered with `BUSY` at `now`.
    pub(crate) fn backing_off(&self, addr: IpAddr, now: Instant) -> bool {
        self.lock()
            .stalls
            .get(&connection_limit_key(addr))
            .is_some_and(|stall| stall.backing_off(now))
    }

    /// Records that a response to `addr` was abandoned at `now` because the
    /// reader fell behind, with a first backoff of `backoff`.
    pub(crate) fn record_stall(&self, addr: IpAddr, now: Instant, backoff: Duration) {
        let addr = connection_limit_key(addr);
        let stalls = &mut self.lock().stalls;
        let stall = Stall::after(stalls.get(&addr).copied(), now, backoff);
        if !stalls.contains_key(&addr) && stalls.len() >= RANGE_STALL_ADDRS {
            stalls.retain(|_addr, stall| !stall.forgotten(now));
        }
        if !stalls.contains_key(&addr) && stalls.len() >= RANGE_STALL_ADDRS {
            let first_to_end = stalls
                .iter()
                .min_by_key(|(_addr, stall)| stall.until)
                .map(|(addr, _stall)| *addr)
                .expect("the table is full, so it is not empty");
            stalls.remove(&first_to_end);
        }
        stalls.insert(addr, stall);
    }
}

/// One stream's share of the node's permits for its address and address
/// group, returned when it is dropped.
#[derive(Debug)]
struct AddrShare {
    node: Arc<NodeRangeLimits>,
    addr: IpAddr,
    group: Group,
}

impl Drop for AddrShare {
    fn drop(&mut self) {
        /// Returns one share of `key`, forgetting keys that hold none.
        fn release<K: std::hash::Hash + Eq>(streams: &mut HashMap<K, usize>, key: &K) {
            if let Some(held) = streams.get_mut(key) {
                *held = held.saturating_sub(1);
                if *held == 0 {
                    streams.remove(key);
                }
            }
        }

        let mut state = self.node.lock();
        release(&mut state.streams_by_addr, &self.addr);
        release(&mut state.streams_by_group, &self.group);
    }
}

/// The backoff of a peer, or an address, whose range responses were
/// abandoned because it fell behind its minimum progress.
#[derive(Copy, Clone, Debug)]
struct Stall {
    /// When the backoff ends.
    until: Instant,
    /// How long the backoff is.
    backoff: Duration,
}

impl Stall {
    /// Whether range streams are answered with `BUSY` at `now`.
    fn backing_off(&self, now: Instant) -> bool {
        now < self.until
    }

    /// Whether the stall no longer affects the next one at `now`.
    fn forgotten(&self, now: Instant) -> bool {
        now >= self.until + RANGE_STALL_BACKOFF_MAX
    }

    /// The backoff after a response is abandoned at `now`, given the
    /// `previous` one: `first` long, or twice the previous one, up to
    /// [`RANGE_STALL_BACKOFF_MAX`], if that is not forgotten yet.
    ///
    /// A response abandoned during a backoff was admitted before it began,
    /// like the one that began it, so it does not add to the backoff: the two
    /// streams a peer may have at once count once when they stall together.
    fn after(previous: Option<Stall>, now: Instant, first: Duration) -> Stall {
        let backoff = match previous {
            Some(previous) if previous.backing_off(now) => return previous,
            Some(previous) if !previous.forgotten(now) => previous
                .backoff
                .saturating_mul(2)
                .min(RANGE_STALL_BACKOFF_MAX),
            _ => first,
        };
        Stall {
            until: now + backoff,
            backoff,
        }
    }
}

/// What serving one peer's `get-block-range` requests needs from its
/// connection.
#[derive(Clone, Debug)]
pub(crate) struct BlockRangeServing {
    /// Whether this node serves `get-block-range` requests at all
    /// (`network.v2_serve_block_ranges`).
    enabled: bool,

    /// The peer's permits, [`RANGE_STREAMS_PER_PEER`] of them.
    peer_permits: Arc<Semaphore>,

    /// The node's serving state, shared by every connection (see
    /// [`global_block_range_permits`]).
    node: Arc<NodeRangeLimits>,

    /// The peer's address, which the node's per-address state is kept for;
    /// `None` keeps only this connection's state.
    remote: Option<IpAddr>,

    /// The backoff of this connection, if one of its responses was abandoned
    /// because the peer fell behind. The lock is never held across an await.
    stall: Arc<Mutex<Option<Stall>>>,

    /// The first backoff after an abandoned response:
    /// [`RANGE_STALL_BACKOFF`], except in tests.
    stall_backoff: Duration,

    /// How long after its stream was accepted a response must be complete:
    /// [`RANGE_STREAM_TIMEOUT`], except in tests.
    stream_timeout: Duration,

    /// How fast the peer must take a response: [`RANGE_MIN_SERVE_RATE`],
    /// with [`RANGE_FIRST_WRITE_TIMEOUT`] to start and
    /// [`RANGE_PROGRESS_WINDOW`] of slack, except in tests.
    min_progress: MinProgress,
}

/// The least progress the reader of a `get-block-range` response must make.
#[derive(Copy, Clone, Debug)]
pub(super) struct MinProgress {
    /// The lowest rate, in bytes per second; zero for no rate floor, where
    /// each write may wait for `window`.
    rate: u64,
    /// How long the first write may wait beyond its bytes' time at `rate`.
    first: Duration,
    /// The most slack a reader ahead of `rate` keeps.
    window: Duration,
}

/// How far ahead of its [`MinProgress`] the reader of one response is.
///
/// The peer is only on the clock while a write waits for it, so the time
/// this node takes to read blocks is never counted against the peer. Each
/// write is allowed the time its bytes take at the floor rate, plus the
/// slack the reader has left, and is abandoned at the moment that runs out:
/// a write that blocks is cut off when the peer falls behind, not at the next
/// check between writes.
#[derive(Debug)]
struct Pace {
    min_progress: MinProgress,
    /// How much longer than the floor rate allows the writes so far could
    /// have waited: the slack the next write starts with.
    slack: Duration,
    /// Whether a write was abandoned because the peer fell behind.
    stalled: bool,
}

impl Pace {
    /// The pace of a response whose request has just been completed.
    fn new(min_progress: MinProgress) -> Self {
        Self {
            min_progress,
            slack: min_progress.first,
            stalled: false,
        }
    }

    /// How long a write of `len` bytes may wait for the peer.
    fn allowance(&self, len: usize) -> Duration {
        let MinProgress { rate, window, .. } = self.min_progress;
        if rate == 0 {
            return window;
        }
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        let at_rate = Duration::from_micros(len.saturating_mul(1_000_000) / rate);
        self.slack.min(window).saturating_add(at_rate)
    }
}

impl BlockRangeServing {
    /// Returns the serving state of a new connection: `enabled` from the
    /// config, fresh permits for the peer, and the `node`'s shared state.
    ///
    /// The node's per-address state and limits are only used once the peer's
    /// address is set with [`BlockRangeServing::for_remote`].
    pub(crate) fn new(enabled: bool, node: Arc<NodeRangeLimits>) -> Self {
        Self {
            enabled,
            peer_permits: Arc::new(Semaphore::new(RANGE_STREAMS_PER_PEER)),
            node,
            remote: None,
            stall: Arc::new(Mutex::new(None)),
            stall_backoff: RANGE_STALL_BACKOFF,
            stream_timeout: RANGE_STREAM_TIMEOUT,
            min_progress: MinProgress {
                rate: RANGE_MIN_SERVE_RATE,
                first: RANGE_FIRST_WRITE_TIMEOUT,
                window: RANGE_PROGRESS_WINDOW,
            },
        }
    }

    /// Returns this serving state for a peer at `remote`, which the node's
    /// per-address state then applies to.
    pub(crate) fn for_remote(mut self, remote: IpAddr) -> Self {
        self.remote = Some(remote);
        self
    }

    /// Returns these serving settings with a first stall backoff of
    /// `stall_backoff` instead of [`RANGE_STALL_BACKOFF`].
    #[cfg(test)]
    pub(crate) fn with_stall_backoff(mut self, stall_backoff: Duration) -> Self {
        self.stall_backoff = stall_backoff;
        self
    }

    /// Returns these serving settings with a whole-stream deadline of
    /// `stream_timeout` instead of [`RANGE_STREAM_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn with_stream_timeout(mut self, stream_timeout: Duration) -> Self {
        self.stream_timeout = stream_timeout;
        self
    }

    /// Returns these serving settings with a minimum rate of `rate` bytes
    /// per second, and `window` both for the first write and as the most
    /// slack, instead of [`RANGE_MIN_SERVE_RATE`],
    /// [`RANGE_FIRST_WRITE_TIMEOUT`] and [`RANGE_PROGRESS_WINDOW`]. A `rate`
    /// of zero lets each write wait for `window`.
    #[cfg(test)]
    pub(crate) fn with_min_progress(mut self, rate: u64, window: Duration) -> Self {
        self.min_progress = MinProgress {
            rate,
            first: window,
            window,
        };
        self
    }

    fn lock_stall(&self) -> std::sync::MutexGuard<'_, Option<Stall>> {
        self.stall
            .lock()
            .expect("the lock is only held to read or replace a value, which does not panic")
    }

    /// Whether the peer's range streams are answered with `BUSY` now,
    /// because a response on this connection, or to its address, was
    /// abandoned for falling behind.
    fn backing_off(&self) -> bool {
        let now = Instant::now();
        self.lock_stall()
            .is_some_and(|stall| stall.backing_off(now))
            || self
                .remote
                .is_some_and(|remote| self.node.backing_off(remote, now))
    }

    /// Records that a response to the peer was abandoned because it fell
    /// behind its minimum progress.
    fn record_stall(&self) {
        let now = Instant::now();
        {
            let mut stall = self.lock_stall();
            *stall = Some(Stall::after(*stall, now, self.stall_backoff));
        }
        if let Some(remote) = self.remote {
            self.node.record_stall(remote, now, self.stall_backoff);
        }
    }

    /// Claims a peer permit for one `get-block-range` stream, before its
    /// request is read, or refuses the stream: with `REFUSED` if serving is
    /// disabled, and with `BUSY` if the peer is backing off after a stalled
    /// response or its permits are exhausted. Never waits.
    pub(super) fn claim_peer(&self) -> Result<PeerRangePermit, Outcome> {
        if !self.enabled {
            tracing::debug!("refusing get-block-range stream: serving is disabled");
            return Err(Outcome::Refuse(ErrorCode::Refused));
        }
        if self.backing_off() {
            return Err(busy("stall backoff"));
        }
        let peer = self
            .peer_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("peer limit"))?;
        Ok(PeerRangePermit(peer))
    }

    /// Adds a node permit to the `peer` permit of a stream whose request was
    /// read and decoded, or refuses the stream with `BUSY` if the node's
    /// permits are exhausted, or the peer's address or address group already
    /// holds its share of them. Never waits.
    ///
    /// # Security
    ///
    /// The node's permits are only claimed for a complete request, so a peer
    /// that opens range streams and never sends their requests holds only its
    /// own permits, and cannot deny range serving to every other peer.
    pub(super) fn claim_node(&self, peer: PeerRangePermit) -> Result<RangePermits, Outcome> {
        let addr = self
            .remote
            .map(|remote| self.node.claim_addr(remote))
            .transpose()?;
        let node = self
            .node
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("node limit"))?;
        Ok(RangePermits {
            _peer: peer.0,
            _node: node,
            _addr: addr,
        })
    }
}

/// Refuses a `get-block-range` stream this node would serve if it were not at
/// a limit, for `reason`: `BUSY` asks the requester to retry after a delay,
/// where `REFUSED` would ask it not to repeat the request.
fn busy(reason: &'static str) -> Outcome {
    tracing::debug!(reason, "refusing get-block-range stream as busy");
    Outcome::Refuse(ErrorCode::Busy)
}

/// The peer permit a `get-block-range` stream holds while its request is
/// read.
#[derive(Debug)]
pub(super) struct PeerRangePermit(OwnedSemaphorePermit);

/// The permits one served `get-block-range` stream holds until it ends.
#[derive(Debug)]
pub(super) struct RangePermits {
    _peer: OwnedSemaphorePermit,
    _node: OwnedSemaphorePermit,
    _addr: Option<AddrShare>,
}

/// A decoded `get-block-range` request waiting to be served, with the
/// permits it was admitted with.
#[derive(Debug)]
pub(super) struct PendingRange {
    /// The request.
    pub(super) request: GetBlockRange,
    /// Held until the response ends.
    pub(super) permits: RangePermits,
}

/// Serves a `get-block-range` request, writing the whole response and
/// finishing the stream, within `serving`'s stream deadline of `accepted` at
/// the latest, each inbound service call within [`REQUEST_TIMEOUT`], and with
/// the peer taking it at `serving`'s minimum progress or faster. A peer that
/// falls behind starts its stall backoff.
///
/// The response is finished early, which the draft allows (the requester
/// resumes from the last block's parent), at genesis, when the inbound
/// service has no more ancestors (a missing or pruned body), and when an
/// inbound service call fails, or does not answer with a block range, after
/// some blocks were written. If that happens on the first call, the stream
/// is refused instead.
pub(super) async fn serve_block_range<S>(
    response: &mut ResponseSend,
    range: PendingRange,
    service: Inbound<'_, S>,
    accepted: Instant,
    serving: &BlockRangeServing,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    let PendingRange {
        request,
        permits: _permits,
    } = range;
    let mut writer = RangeWriter {
        response,
        stream_deadline: accepted + serving.stream_timeout,
        pace: Pace::new(serving.min_progress),
    };
    let served = write_block_range(&mut writer, &request, service).await;
    if writer.pace.stalled {
        serving.record_stall();
    }
    served?;

    metrics::counter!(
        "zebra.net.out.responses",
        "command" => StreamType::GetBlockRange.name(),
    )
    .increment(1);

    // `finish` only fails if the stream was already closed, which the peer may do at will.
    let _ = writer.response.send.finish();
    Ok(())
}

/// Writes the whole response to `request`.
async fn write_block_range<S>(
    writer: &mut RangeWriter<'_>,
    request: &GetBlockRange,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    let mut progress = Progress {
        next: request.final_hash,
        delivered: 0,
        delivered_bytes: 0,
        raw: Vec::new(),
        element: Vec::new(),
    };

    // `count = 0` is a valid request, answered with no blocks.
    if request.count == 0 {
        let mut answer = Vec::with_capacity(1);
        write_block_range_answer(&mut answer, BlockRangeAnswer::Found)?;
        writer.write(&answer).await?;
    }

    while progress.delivered < request.count {
        match serve_chunk(writer, request, service.clone(), &mut progress).await? {
            Continue::More => {}
            Continue::Done => break,
        }
    }
    Ok(())
}

/// The response stream of one `get-block-range` request, with the bounds on
/// how long its writes may wait for the peer.
struct RangeWriter<'a> {
    response: &'a mut ResponseSend,
    /// When the whole response must be complete.
    stream_deadline: Instant,
    /// How far ahead of its minimum progress the peer is.
    pace: Pace,
}

impl RangeWriter<'_> {
    /// Writes `bytes` to the response stream, or abandons the response if
    /// the peer does not take them within its [`Pace`], or by the stream
    /// deadline.
    async fn write(&mut self, bytes: &[u8]) -> Result<(), Outcome> {
        let allowance = self.pace.allowance(bytes.len());
        let started = Instant::now();
        let behind_at = started + allowance;
        let written = tokio::time::timeout_at(
            behind_at.min(self.stream_deadline),
            self.response.write(bytes),
        )
        .await;
        match written {
            Ok(result) => {
                self.pace.slack = allowance.saturating_sub(started.elapsed());
                result
            }
            Err(_elapsed) => {
                self.pace.stalled = behind_at <= self.stream_deadline;
                tracing::debug!(
                    stalled = self.pace.stalled,
                    ?allowance,
                    "get-block-range response was not taken in time, abandoning it"
                );
                Err(Outcome::Refuse(self.response.abandon_code()))
            }
        }
    }
}

/// How far a `get-block-range` response has got.
struct Progress {
    /// The hash of the next block to send.
    next: block::Hash,
    /// How many blocks were written.
    delivered: u32,
    /// How many serialized block bytes were written, the first block
    /// included.
    delivered_bytes: u64,
    /// A reused buffer for one block's serialization.
    raw: Vec<u8>,
    /// A reused buffer for one response element.
    element: Vec<u8>,
}

/// Whether a `get-block-range` response continues after a chunk.
enum Continue {
    /// Read and write the next chunk.
    More,
    /// The response is complete: finish the stream.
    Done,
}

/// Reads one chunk of the range from the inbound service and writes it.
async fn serve_chunk<S>(
    writer: &mut RangeWriter<'_>,
    request: &GetBlockRange,
    service: Inbound<'_, S>,
    progress: &mut Progress,
) -> Result<Continue, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let first = progress.delivered == 0;
    let count = request
        .count
        .saturating_sub(progress.delivered)
        .min(RANGE_READ_CHUNK_BLOCKS);
    // # Security
    //
    // Every block after the first must fit in what is left of `max_bytes`, so no read asks for
    // more than that: a request with a small `max_bytes` makes a small read, not a whole chunk.
    // The inbound service always returns a read's first block, so the response's first block is
    // sent whatever `max_bytes` is.
    let left = u64::from(request.max_bytes).saturating_sub(progress.delivered_bytes);
    if !first && left == 0 {
        return Ok(Continue::Done);
    }
    let max_bytes = u32::try_from(left)
        .expect("left is at most request.max_bytes, which is a u32")
        .min(RANGE_READ_CHUNK);

    // # Security
    //
    // The read is bounded on its own, so a read that never answers does not hold the stream's
    // permits until the stream deadline. The peer is not on the clock while it runs.
    let read_deadline = (Instant::now() + REQUEST_TIMEOUT).min(writer.stream_deadline);
    let answer = tokio::time::timeout_at(
        read_deadline,
        call_service(
            service,
            Request::BlockRange {
                from_hash: progress.next,
                count,
                max_bytes,
            },
        ),
    )
    .await
    .map_err(|_elapsed| {
        tracing::debug!(
            delivered = progress.delivered,
            delivered_bytes = progress.delivered_bytes,
            "get-block-range read missed its deadline, abandoning the response"
        );
        Outcome::Refuse(writer.response.abandon_code())
    })?;
    let blocks = match answer {
        Ok(Response::BlockRange(blocks)) => blocks,
        // Any other answer, such as the `Nil` of an inbound service that is still starting, says
        // nothing about whether this node has the blocks, so it must not be answered as not
        // found: the stream is refused, and the requester asks another peer.
        Ok(other) if first => {
            tracing::debug!(
                response = %other,
                "inbound service did not answer a block range read, refusing the stream"
            );
            return Err(Outcome::Refuse(ErrorCode::Refused));
        }
        // Blocks are already written: finish early so the requester keeps them.
        Ok(other) => {
            tracing::debug!(
                response = %other,
                "inbound service did not answer a block range read part way, finishing early"
            );
            return Ok(Continue::Done);
        }
        // Blocks are already written: finish early so the requester keeps them.
        Err(Outcome::Refuse(code)) if !first => {
            tracing::debug!(%code, "block range read failed part way, finishing early");
            return Ok(Continue::Done);
        }
        Err(error) => return Err(error),
    };

    if blocks.is_empty() {
        if first {
            // This node does not have `final_hash`: a not-found answer, with nothing following.
            progress.element.clear();
            write_block_range_answer(&mut progress.element, BlockRangeAnswer::NotFound)?;
            writer.write(&progress.element).await?;
        }
        // Otherwise the inbound service has no more ancestors: finish early.
        return Ok(Continue::Done);
    }

    // Each block is dropped once it is written, so a chunk's memory is freed as it goes.
    // `count` is at most `RANGE_READ_CHUNK_BLOCKS`, which fits in any `usize`.
    for block in blocks.into_iter().take(count as usize) {
        // # Correctness
        //
        // The inbound service returns an ancestor chain, but a block that does not chain would
        // make the requester close the connection and blame this node, so the response is
        // finished before it instead.
        if block.hash() != progress.next {
            tracing::warn!(
                expected = %progress.next,
                got = %block.hash(),
                "inbound service returned a block outside the requested ancestor chain, \
                 finishing the get-block-range response early"
            );
            if progress.delivered == 0 {
                progress.element.clear();
                write_block_range_answer(&mut progress.element, BlockRangeAnswer::NotFound)?;
                writer.write(&progress.element).await?;
            }
            return Ok(Continue::Done);
        }

        progress.raw.clear();
        block
            .zcash_serialize(&mut progress.raw)
            .map_err(WireError::Io)?;
        // `usize` fits in `u64` on every supported platform.
        let block_len = progress.raw.len() as u64;

        // Both bounds are exact, but the first block is always sent (draft, "`get-block-range`").
        if progress.delivered > 0
            && progress.delivered_bytes.saturating_add(block_len) > u64::from(request.max_bytes)
        {
            return Ok(Continue::Done);
        }

        progress.element.clear();
        if progress.delivered == 0 {
            write_block_range_answer(&mut progress.element, BlockRangeAnswer::Found)?;
        }
        write_block_range_element(&mut progress.element, &progress.raw)?;
        writer.write(&progress.element).await?;

        progress.delivered += 1;
        progress.delivered_bytes = progress.delivered_bytes.saturating_add(block_len);
        progress.next = block.header.previous_block_hash;

        // The genesis block has no parent to continue from.
        if progress.next == GENESIS_PREVIOUS_BLOCK_HASH {
            return Ok(Continue::Done);
        }
    }

    Ok(if progress.delivered < request.count {
        Continue::More
    } else {
        Continue::Done
    })
}
