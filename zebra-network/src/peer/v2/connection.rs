//! The version 2 peer connection task.
//!
//! A [`Connection`] maps the internal [`Request`]/[`Response`] protocol onto
//! version 2 streams:
//!
//! - Each outbound internal request opens its own request stream, so
//!   requests are correlated structurally, and are cancelled and timed out
//!   per stream. Unlike the legacy connection, requests run concurrently.
//! - Each inbound request stream is decoded, served by the inbound service,
//!   and answered on the same stream.
//! - Announcements (blocks, transactions, addresses) are written to and read
//!   from long-lived unidirectional announcement streams. Outbound
//!   announcements are best-effort: they are dropped rather than queued
//!   indefinitely when the transport applies backpressure.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use futures::StreamExt;
use indexmap::{IndexMap, IndexSet};
use tower::{Service, ServiceExt};
use tracing::{debug, warn, Instrument};

use zebra_chain::{
    block::{self, merkle, Block},
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{Transaction, UnminedTx, UnminedTxId, WtxId},
};

use crate::{
    constants,
    meta_addr::MetaAddr,
    peer::{
        client::{ClientRequestReceiver, InProgressClientRequest, MustUseClientResponseSender},
        connection::update_addr_cache,
        ErrorSlot, PeerError, SharedPeerError,
    },
    peer_set::{ConnectionTracker, InventoryChange, SlotCounter, SlotGuard},
    protocol::{
        external::InventoryHash,
        internal::{InventoryResponse, Request, Response},
        v2::{
            compact_block::{
                full_transaction_id, short_id_keys, short_transaction_id, CompactBlock,
                CompactBlockIds, ShortTxId,
            },
            constants::{
                BLOCK_ANNOUNCEMENT_KIND_COMPACT, BLOCK_ANNOUNCEMENT_KIND_HEADER,
                HEADERS_ENTRY_HAS_TXS, HEADERS_ENTRY_NO_TXS, INBOUND_STREAM_TIMEOUT,
                MAX_CONCURRENT_BULK_STREAMS, MAX_CONSECUTIVE_REQUEST_TIMEOUTS,
                MAX_MEMPOOL_RECORD_REFS, MAX_MEMPOOL_RESPONSE_REFS, MAX_RECORD_PAYLOAD_LEN,
                MISBEHAVIOR_PENALTY_INVALID_POW, TX_TRICKLE_MEAN_INTERVAL,
            },
            init::{HandshakeRecord, InitRecord},
            record,
            request::Request as WireRequest,
            response::{
                AddrResponse, BlockResponseEntry, HashesResponse, HeadersResponse, MempoolResponse,
                TreeRootsResponse, TxResponseEntry, RESULT_NOT_FOUND, RESULT_OBJECT,
            },
            txref::TransactionReference,
            types::{ErrorCode, StreamType, WireError},
        },
    },
    BoxError, PeerSocketAddr,
};

/// The maximum number of outbound announcement records queued for writing.
///
/// Announcements are best-effort: when the queue is full, new announcements
/// are dropped rather than queued indefinitely.
pub(super) const ANNOUNCEMENT_QUEUE_LIMIT: usize = 64;

/// The capacity of the per-connection mempool update channel feeding the
/// peer's `get-mempool` subscription.
///
/// When the subscription serving task falls this far behind, it recovers by
/// re-sending a whole mempool snapshot in place of the lost updates.
pub(super) const MEMPOOL_UPDATES_CHANNEL_CAPACITY: usize = 64;

/// The maximum number of transaction announcements queued for a
/// connection's next trickle flush.
///
/// Announcements are best-effort: beyond the bound, the oldest queued
/// announcement is dropped. The peer can still learn of dropped
/// transactions from its `get-tx` responses or a later `get-mempool`
/// snapshot.
const PENDING_TX_ANNOUNCEMENT_LIMIT: usize = 5_000;

/// The maximum number of recently pushed transactions kept for serving
/// `get-tx` requests.
///
/// The v2 protocol has no unsolicited transaction push: pushed transactions
/// are announced, and the peer fetches them with `get-tx`. This cache serves
/// those fetches even if the transaction has not reached the local mempool.
const PUSHED_TRANSACTION_CACHE_LIMIT: usize = 32;

/// The maximum number of recently sent blocks kept per connection for
/// serving the peer's reconstruction `get-tx` requests.
///
/// A node that sends a compact block, or a `get-headers` response entry with
/// transaction IDs, must be able to serve that block's transactions to the
/// same peer for a reasonable time afterwards, even if they leave its
/// mempool. This cache is that time bound: at most this many blocks are
/// retained per peer, so the total memory bound is this limit times the peer
/// count (blocks near the tip are also usually shared between the caches).
const SENT_BLOCK_CACHE_LIMIT: usize = 8;

/// The maximum number of blocks reconstructed from compact block
/// announcements kept per connection, to serve the local gossip
/// downloader's follow-up block request without a wire round-trip.
///
/// Only connections on which this node requested high-bandwidth
/// announcements fill this cache, so its memory bound is this limit times
/// [`MAX_HIGH_BANDWIDTH_PEERS`].
const RECONSTRUCTED_BLOCK_CACHE_LIMIT: usize = 4;

/// The read buffer size for outbound request streams, whose responses can
/// carry multiple megabytes of blocks or transactions.
///
/// Responses are decoded with many small reads, and every unbuffered read of
/// a QUIC stream locks shared transport state.
const RESPONSE_STREAM_READ_BUFFER_SIZE: usize = 64 * 1024;

/// The read buffer size for inbound request streams.
///
/// Inbound requests are small (only `get-tx` exceeds a few KiB), and the
/// remote peer can hold many request streams open concurrently, so a large
/// per-stream buffer would let it pin megabytes of allocations per
/// connection.
const INBOUND_REQUEST_STREAM_READ_BUFFER_SIZE: usize = 8 * 1024;

/// The read buffer size for the long-lived handshake and announcement
/// streams.
///
/// Their records are small and infrequent, and each open stream keeps its
/// buffer for the life of the connection.
const RECORD_STREAM_READ_BUFFER_SIZE: usize = 8 * 1024;

/// An announcement record queued for the announcement writer task.
pub(super) type AnnouncementRecord = (StreamType, Vec<u8>);

/// Collects the blocks an inventory [`Response`] made available, keyed by
/// hash.
fn available_blocks(response: Response) -> IndexMap<block::Hash, Arc<Block>> {
    let Response::Blocks(blocks) = response else {
        return IndexMap::new();
    };

    blocks
        .into_iter()
        .filter_map(|entry| entry.available())
        .map(|(block, _)| (block.hash(), block))
        .collect()
}

/// Inserts `key` into a bounded most-recently-used cache, evicting the
/// oldest entries until `limit` is respected.
///
/// Re-inserting an existing key moves it to the most recent slot.
fn insert_bounded<K: std::hash::Hash + Eq, V>(
    cache: &mut IndexMap<K, V>,
    key: K,
    value: V,
    limit: usize,
) {
    cache.shift_remove(&key);
    while cache.len() >= limit {
        cache.shift_remove_index(0);
    }
    cache.insert(key, value);
}

/// State shared between the connection task and the per-stream tasks it
/// A mutable value shared between the connection task and the per-stream
/// tasks it spawns.
///
/// Backed by a [`watch`](tokio::sync::watch) channel used as a cell: updates
/// run atomically inside [`send_modify`](tokio::sync::watch::Sender::send_modify),
/// and reads take a short borrow snapshot.
pub(super) struct SharedCell<T>(tokio::sync::watch::Sender<T>);

impl<T: Default> Default for SharedCell<T> {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(T::default()).0)
    }
}

impl<T> SharedCell<T> {
    /// Runs `f` on the shared value, atomically with other updates.
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut result = None;
        self.0.send_modify(|value| result = Some(f(value)));
        result.expect("send_modify runs the closure exactly once")
    }

    /// Returns a read snapshot of the shared value.
    ///
    /// The snapshot must be dropped before any await point, like a lock
    /// guard.
    fn read(&self) -> tokio::sync::watch::Ref<'_, T> {
        self.0.borrow()
    }
}

/// spawns.
pub(super) struct SharedConnection {
    /// The QUIC connection.
    pub(super) quic: quinn::Connection,

    /// The shared error slot for this connection.
    pub(super) error_slot: ErrorSlot,

    /// The remote peer's `init` record.
    pub(super) remote_init: Arc<InitRecord>,

    /// Whether this node requested transaction relay in its own `init`
    /// record.
    pub(super) local_relay: bool,

    /// The high-bandwidth announcement slot this connection claimed, if it
    /// requested high-bandwidth compact block announcements in its own
    /// `init` record; dropping it releases the slot to a replacement
    /// connection.
    ///
    /// Holding a slot is exactly this connection's `announce` preference,
    /// so the two can never disagree.
    pub(super) hb_slot: Option<SlotGuard>,

    /// The remote peer's address, used to identify it in the inventory
    /// registry and the inbound service.
    pub(super) transient_addr: Option<PeerSocketAddr>,

    /// Whether the remote peer initiated this connection.
    pub(super) is_inbound: bool,

    /// Registers inventory advertised or missing on this connection.
    pub(super) inv_collector: tokio::sync::broadcast::Sender<InventoryChange>,

    /// Sends misbehavior scores for this peer, so it is banned once it
    /// reaches the ban threshold.
    pub(super) misbehavior_tx: tokio::sync::mpsc::Sender<(PeerSocketAddr, u32)>,

    /// Queues outbound announcement records for the announcement writer
    /// task.
    pub(super) announcement_tx: tokio::sync::mpsc::Sender<AnnouncementRecord>,

    /// Recently pushed transactions, served to the peer's `get-tx` requests.
    pub(super) pushed_transactions: SharedCell<IndexMap<UnminedTxId, UnminedTx>>,

    /// Blocks recently sent to this peer with their transactions identified
    /// rather than included — as compact block announcements or `get-headers`
    /// entries with transaction IDs — retained to serve the peer's
    /// reconstruction `get-tx` requests, including by `SHORTID` reference.
    pub(super) sent_blocks: SharedCell<IndexMap<block::Hash, SentBlock>>,

    /// Blocks reconstructed from this peer's compact block announcements,
    /// serving the local gossip downloader's follow-up block requests
    /// without a wire round-trip.
    pub(super) reconstructed_blocks: SharedCell<IndexMap<block::Hash, Arc<Block>>>,

    /// Broadcasts transactions newly accepted into the local mempool to the
    /// peer's `get-mempool` subscription, if one is active.
    ///
    /// Fed by the connection's trickle task, so subscription updates share
    /// the announcement stream's trickling.
    pub(super) mempool_updates: tokio::sync::broadcast::Sender<Vec<UnminedTxId>>,

    /// Transactions queued for announcement to this peer at the next
    /// trickle flush.
    pub(super) pending_tx_announcements: SharedCell<IndexSet<UnminedTxId>>,

    /// Whether the peer currently has a `get-mempool` subscription open;
    /// a second concurrent subscription is a connection error.
    pub(super) mempool_subscribed: std::sync::atomic::AtomicBool,

    /// The remote peer's mempool transaction IDs, mirrored by this node's
    /// own `get-mempool` subscription to the peer, answering internal
    /// [`Request::MempoolTransactionIds`] without a wire round-trip.
    ///
    /// Bounded at [`MAX_MEMPOOL_RESPONSE_REFS`] entries, evicting the
    /// oldest half beyond the bound: removals from the remote mempool are
    /// never communicated, so old entries only expire by eviction or
    /// disconnection.
    pub(super) remote_mempool: SharedCell<IndexSet<UnminedTxId>>,

    /// Peer addresses announced by the remote peer, served to internal
    /// [`Request::Peers`] requests, mirroring the legacy connection's
    /// address cache.
    pub(super) cached_addrs: SharedCell<Vec<MetaAddr>>,

    /// Rate-limits the relayed addresses accepted from this peer's address
    /// announcement stream.
    pub(super) addr_tokens: SharedCell<AddrTokenBucket>,

    /// The announcement stream types the remote peer currently has open
    /// towards this node, to enforce the one-stream-per-type rule.
    pub(super) open_inbound_announcements: SharedCell<HashSet<u8>>,

    /// Whether this node has already sent a `get-addr` request on this
    /// connection.
    ///
    /// To impede address-based fingerprinting, `get-addr` is sent at most
    /// once per connection; later address requests are answered from the
    /// address cache.
    pub(super) sent_get_addr: std::sync::atomic::AtomicBool,

    /// The number of outbound requests that have timed out since the last
    /// response from this peer.
    pub(super) consecutive_timeouts: AtomicU32,

    /// The bulk block streams (`get-block-range`) this node is currently
    /// serving to the peer.
    pub(super) active_bulk_streams: SlotCounter,

    /// The directory of content-addressed synchronization artifacts served
    /// to the peer's `get-object` requests, when this node has one.
    pub(super) artifact_dir: Option<Arc<std::path::PathBuf>>,
}

/// A token bucket rate-limiting the relayed addresses accepted from one
/// connection.
///
/// Addresses beyond the sustained rate and burst capacity are silently
/// dropped: rate limiting is not a penalty.
#[derive(Debug)]
pub(super) struct AddrTokenBucket {
    /// The available tokens; one address costs one token.
    tokens: f64,

    /// When the bucket was last refilled.
    last_refill: std::time::Instant,
}

impl Default for AddrTokenBucket {
    fn default() -> Self {
        AddrTokenBucket {
            tokens: crate::protocol::v2::constants::ADDR_TOKEN_BUCKET_CAPACITY,
            last_refill: std::time::Instant::now(),
        }
    }
}

impl AddrTokenBucket {
    /// Takes one token at `now`, refilling at the reference rate first.
    ///
    /// Returns whether the address should be processed.
    pub(super) fn try_take(&mut self, now: std::time::Instant) -> bool {
        use crate::protocol::v2::constants::{ADDR_TOKEN_BUCKET_CAPACITY, ADDR_TOKEN_RATE};

        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * ADDR_TOKEN_RATE).min(ADDR_TOKEN_BUCKET_CAPACITY);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A block recently sent to the peer with its transactions identified rather
/// than included, retained to serve reconstruction `get-tx` requests.
pub(super) struct SentBlock {
    /// The block.
    pub(super) block: Arc<Block>,

    /// The short transaction ID nonce of the compact block sent for this
    /// block, if one was sent.
    ///
    /// `SHORTID` references are resolved with the nonce of the compact block
    /// most recently sent to the requesting peer for the block. Blocks sent
    /// only as `get-headers` entries carry full transaction IDs, so they
    /// have no nonce, and `SHORTID` references to them are answered
    /// not-found.
    pub(super) nonce: Option<u64>,
}

impl SharedConnection {
    /// Records that a block was sent to this peer with its transactions
    /// identified rather than included, so its transactions can be served to
    /// the peer's reconstruction `get-tx` requests.
    ///
    /// Callers record the block before the announcement or response entry is
    /// written, so the obligation is servable as soon as the peer can
    /// request it.
    pub(super) fn record_sent_block(&self, block: Arc<Block>, nonce: Option<u64>) {
        let hash = block.hash();
        // Re-recording moves the block to the most recent slot, and a
        // compact block's nonce replaces a `get-headers` entry's absent
        // nonce.
        self.sent_blocks.with(|sent| {
            insert_bounded(
                sent,
                hash,
                SentBlock { block, nonce },
                SENT_BLOCK_CACHE_LIMIT,
            )
        });
    }

    /// Fails the connection: records `error` in the error slot and closes
    /// the connection with `code`.
    pub(super) fn fail(&self, code: ErrorCode, error: PeerError) {
        let message = error.to_string();
        // Ignore errors updating the slot: the earliest error wins.
        let _ = self.error_slot.try_update_error(error.into());
        self.quic.close(code.into(), message.as_bytes());
    }

    /// Fails the connection with a `PROTOCOL_ERROR`: the peer violated the
    /// v2 protocol.
    pub(super) fn fail_protocol(&self, reason: impl Into<String>) {
        self.fail(
            ErrorCode::ProtocolError,
            PeerError::V2Protocol(reason.into()),
        );
    }

    /// Fails the connection for a violation reported as a [`WireError`],
    /// closing it with the error's connection error code.
    pub(super) fn fail_wire_error(&self, error: &WireError) {
        let code = error
            .connection_error_code()
            .unwrap_or(ErrorCode::ProtocolError);
        self.fail(code, PeerError::V2Protocol(error.to_string()));
    }

    /// Records that this peer answered a request, clearing the consecutive
    /// timeout count.
    pub(super) fn record_response(&self) {
        self.consecutive_timeouts.store(0, Ordering::Relaxed);
    }

    /// Records that a request to this peer timed out, and fails the
    /// connection once [`MAX_CONSECUTIVE_REQUEST_TIMEOUTS`] are reached.
    pub(super) fn record_request_timeout(&self) {
        let timeouts = self.consecutive_timeouts.fetch_add(1, Ordering::Relaxed) + 1;

        if timeouts >= MAX_CONSECUTIVE_REQUEST_TIMEOUTS {
            // A responder must answer a request stream it accepted, or reset
            // it, so a peer that does neither is not following the protocol.
            self.fail_protocol(format!(
                "peer did not answer {timeouts} consecutive requests"
            ));
        }
    }

    /// Assigns a misbehavior score to this peer.
    ///
    /// Unlike [`fail`](Self::fail), this leaves the connection open: the peer
    /// is disconnected and banned by the address book once its accumulated
    /// score reaches
    /// [`MAX_PEER_MISBEHAVIOR_SCORE`](crate::constants::MAX_PEER_MISBEHAVIOR_SCORE).
    pub(super) fn report_misbehavior(&self, points: u32, reason: &str) {
        let Some(addr) = self.transient_addr else {
            return;
        };

        debug!(?addr, points, reason, "v2 peer misbehaved");

        // The channel has room for one update per peer connection, so a full
        // channel means the updates are already being batched.
        if self.misbehavior_tx.try_send((addr, points)).is_err() {
            debug!(?addr, "dropping a v2 misbehavior update: channel is full");
        }
    }

    /// Queues an announcement record for the writer task, dropping it if the
    /// queue is full.
    pub(super) fn enqueue_announcement(&self, stream_type: StreamType, payload: Vec<u8>) {
        if self
            .announcement_tx
            .try_send((stream_type, payload))
            .is_err()
        {
            debug!(?stream_type, "dropping queued announcement");
        }
    }
}

/// The version 2 peer connection task state.
pub struct Connection<S> {
    /// State shared with the per-stream tasks.
    pub(super) shared: Arc<SharedConnection>,

    /// The service that handles requests from the remote peer.
    pub(super) inbound_service: S,

    /// Receives internal requests from the [`Client`](crate::peer::Client)
    /// half of the connection.
    pub(super) client_rx: ClientRequestReceiver,

    /// The receiving half of the handshake stream, monitored for control
    /// records and disconnection for the life of the connection.
    pub(super) handshake_recv: quinn::RecvStream,

    /// The sending half of the handshake stream, held open for the life of
    /// the connection: finishing it would signal intent to disconnect.
    pub(super) handshake_send: quinn::SendStream,

    /// The receiving half of the announcement queue.
    pub(super) announcement_rx: tokio::sync::mpsc::Receiver<AnnouncementRecord>,

    /// Keeps the connection counted while this connection is open.
    pub(super) connection_tracker: ConnectionTracker,
}

impl<S> Connection<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    /// Runs the connection until it fails, the peer disconnects, or the
    /// [`Client`](crate::peer::Client) is dropped.
    pub async fn run(self) {
        let Connection {
            shared,
            inbound_service,
            mut client_rx,
            handshake_recv,
            handshake_send,
            announcement_rx,
            connection_tracker,
        } = self;

        // The handshake send half is held open for the life of the
        // connection: finishing it would signal intent to disconnect.
        let _handshake_send = handshake_send;

        // The handshake stream monitor and announcement writer run as
        // separate tasks, because their reads and writes are not
        // cancellation-safe inside a select loop.
        let handshake_monitor = tokio::spawn(
            monitor_handshake_stream(shared.clone(), handshake_recv).in_current_span(),
        );
        let announcement_writer = tokio::spawn(
            write_announcements(shared.quic.clone(), announcement_rx).in_current_span(),
        );

        let quic = shared.quic.clone();

        let exit_error: PeerError = loop {
            tokio::select! {
                client_request = client_rx.next() => match client_request {
                    Some(request) => {
                        dispatch_client_request(&shared, &inbound_service, request)
                    }
                    // The Client was dropped: this node is disconnecting.
                    None => break PeerError::ClientDropped,
                },

                bi_stream = quic.accept_bi() => match bi_stream {
                    Ok((send, recv)) => {
                        let shared = shared.clone();
                        let inbound_service = inbound_service.clone();
                        tokio::spawn(
                            serve_inbound_request_stream(shared, inbound_service, send, recv)
                                .in_current_span(),
                        );
                    }
                    Err(error) => break connection_error_to_peer_error(error),
                },

                uni_stream = quic.accept_uni() => match uni_stream {
                    Ok(recv) => {
                        let shared = shared.clone();
                        let inbound_service = inbound_service.clone();
                        tokio::spawn(
                            read_inbound_announcement_stream(shared, inbound_service, recv)
                                .in_current_span(),
                        );
                    }
                    Err(error) => break connection_error_to_peer_error(error),
                },
            }
        };

        debug!(?exit_error, "closing v2 peer connection");

        // Record the exit error for the Client, and close the connection.
        // The earliest recorded error wins, so a specific failure recorded
        // by a stream task is not overwritten.
        let _ = shared.error_slot.try_update_error(exit_error.into());
        quic.close(ErrorCode::NoError.into(), b"disconnecting");

        handshake_monitor.abort();
        announcement_writer.abort();

        // Fail any queued client requests.
        while let Some(InProgressClientRequest { tx, span, .. }) = client_rx.close_and_flush_next()
        {
            let _entered = span.enter();
            let error = shared
                .error_slot
                .try_get_error()
                .expect("the error slot was just set");
            let _ = tx.send(Err(error));
        }

        // The connection is closed: release its connection count.
        std::mem::drop(connection_tracker);
    }
}

/// Converts a QUIC connection error into the [`PeerError`] reported to the
/// [`Client`](crate::peer::Client).
fn connection_error_to_peer_error(error: quinn::ConnectionError) -> PeerError {
    match error {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            let code = ErrorCode::from_wire(closed.error_code.into());
            PeerError::V2Protocol(format!(
                "connection closed by peer with {code:?}: {}",
                String::from_utf8_lossy(&closed.reason),
            ))
        }
        quinn::ConnectionError::LocallyClosed => PeerError::ConnectionClosed,
        quinn::ConnectionError::TimedOut => PeerError::ConnectionReceiveTimeout,
        other => PeerError::V2Protocol(format!("connection error: {other}")),
    }
}

/// Monitors the handshake stream for the life of the connection.
///
/// Records of unrecognized kinds are ignored, so future revisions can define
/// new control records. A second `init` record is a connection error of type
/// `PROTOCOL_ERROR`. Finishing the handshake stream signals intent to
/// disconnect, and the connection is closed with `NO_ERROR`.
async fn monitor_handshake_stream(
    shared: Arc<SharedConnection>,
    handshake_recv: quinn::RecvStream,
) {
    let mut handshake_recv =
        tokio::io::BufReader::with_capacity(RECORD_STREAM_READ_BUFFER_SIZE, handshake_recv);

    loop {
        let record = record::read_record_timeout(&mut handshake_recv, INBOUND_STREAM_TIMEOUT)
            .await
            .and_then(|payload| payload.as_deref().map(HandshakeRecord::parse).transpose());

        match record {
            Ok(Some(HandshakeRecord::Init(_))) => {
                shared.fail_protocol("peer sent more than one init record");
                break;
            }
            Ok(Some(HandshakeRecord::Unknown(kind))) => {
                debug!(kind, "ignoring unrecognized handshake stream record");
            }
            // The peer finished the handshake stream: it is disconnecting.
            Ok(None) => {
                let _ = shared
                    .error_slot
                    .try_update_error(PeerError::ConnectionClosed.into());
                shared
                    .quic
                    .close(ErrorCode::NoError.into(), b"peer disconnected");
                break;
            }
            // The connection failed or was closed; the connection task
            // reports the error.
            Err(WireError::Io(_)) => break,
            Err(error) => {
                shared.fail_wire_error(&error);
                break;
            }
        }
    }
}

/// Writes queued announcement records to their announcement streams, opening
/// each stream lazily on first use.
///
/// A single writer task per connection enforces the rule that a node opens
/// at most one announcement stream of a given type in each direction.
async fn write_announcements(
    quic: quinn::Connection,
    mut announcement_rx: tokio::sync::mpsc::Receiver<AnnouncementRecord>,
) {
    let mut streams: IndexMap<u8, quinn::SendStream> = IndexMap::new();

    while let Some((stream_type, payload)) = announcement_rx.recv().await {
        let send = match streams.entry(stream_type.byte()) {
            indexmap::map::Entry::Occupied(entry) => entry.into_mut(),
            indexmap::map::Entry::Vacant(entry) => {
                let mut send = match quic.open_uni().await {
                    Ok(send) => send,
                    // The connection is closing.
                    Err(_) => break,
                };
                if send.write_all(&[stream_type.byte()]).await.is_err() {
                    continue;
                }
                entry.insert(send)
            }
        };

        let mut framed = Vec::with_capacity(payload.len() + 5);
        if record::write_record(&mut framed, &payload).is_err() {
            continue;
        }

        if send.write_all(&framed).await.is_err() {
            // The stream was reset or stopped; a replacement stream may be
            // opened for the next announcement.
            streams.shift_remove(&stream_type.byte());
        }
    }
}

/// Dispatches an internal request from the [`Client`](crate::peer::Client).
///
/// Announcements and locally-answerable requests are handled inline;
/// requests that need a response from the peer spawn a request stream task.
fn dispatch_client_request<S>(
    shared: &Arc<SharedConnection>,
    inbound_service: &S,
    request: InProgressClientRequest,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let InProgressClientRequest { request, tx, span } = request;
    let _entered = span.enter();

    if tx.is_canceled() {
        return;
    }

    match request {
        // The v2 protocol has no ping: the transport provides keep-alives and
        // round-trip time measurement, so heartbeats are answered locally.
        //
        // This means heartbeats cannot detect a peer whose QUIC stack answers
        // keep-alives but whose application never answers a request stream.
        // Consecutive request timeouts disconnect that peer instead, see
        // [`SharedConnection::record_request_timeout`].
        Request::Ping(_) => {
            let _ = tx.send(Ok(Response::Pong(shared.quic.rtt())));
        }

        // Cached addresses from the peer's address announcements answer
        // address requests without a wire round-trip, mirroring the legacy
        // connection's address cache.
        Request::Peers => {
            // Security: this method performs security-sensitive operations, see its comments
            // for details.
            let cached: Vec<MetaAddr> = shared.cached_addrs.with(|cached_addrs| {
                update_addr_cache(cached_addrs, None, constants::PEER_ADDR_RESPONSE_LIMIT)
            });

            // To impede fingerprinting, a node only answers address requests
            // on connections the remote peer initiated, and sends `get-addr`
            // at most once per connection. So only ask peers this node
            // connected to and has not asked before: asking the others
            // always fails, or repeats a request the peer may refuse.
            //
            // The short-circuit matters: the sent flag is only set when this
            // request is actually sent to the peer.
            let should_request = cached.is_empty()
                && !shared.is_inbound
                && !shared
                    .sent_get_addr
                    .swap(true, std::sync::atomic::Ordering::Relaxed);

            if should_request {
                spawn_outbound_request(shared, Request::Peers, tx, span.clone());
            } else {
                let _ = tx.send(Ok(Response::Peers(cached)));
            }
        }

        // v2 has no unsolicited transaction push: cache the transaction for
        // the peer's `get-tx`, and announce it.
        Request::PushTransaction(transaction, _) => {
            shared.pushed_transactions.with(|pushed| {
                insert_bounded(
                    pushed,
                    transaction.id,
                    transaction.clone(),
                    PUSHED_TRANSACTION_CACHE_LIMIT,
                )
            });

            queue_transaction_announcements(shared, [transaction.id]);
            let _ = tx.send(Ok(Response::Nil));
        }

        Request::AdvertiseTransactionIds(ids, _) => {
            queue_transaction_announcements(shared, ids);
            let _ = tx.send(Ok(Response::Nil));
        }

        Request::AdvertiseBlock(hash, _) | Request::AdvertiseBlockToAll(hash) => {
            let shared = shared.clone();
            let inbound_service = inbound_service.clone();
            tokio::spawn(announce_block(shared, inbound_service, hash).in_current_span());
            let _ = tx.send(Ok(Response::Nil));
        }

        request => spawn_outbound_request(shared, request, tx, span.clone()),
    }
}

/// Queues `ids` for the connection's next transaction trickle flush.
fn queue_transaction_announcements(
    shared: &SharedConnection,
    ids: impl IntoIterator<Item = UnminedTxId>,
) {
    shared.pending_tx_announcements.with(|pending| {
        for id in ids {
            // Announcements are best-effort and unordered, so the bound evicts
            // an arbitrary old entry.
            if pending.len() >= PENDING_TX_ANNOUNCEMENT_LIMIT {
                pending.swap_remove_index(0);
            }
            pending.insert(id);
        }
    });
}

/// Trickles this connection's transaction announcements: queued
/// announcements are flushed in one batch after a random delay, to impede
/// network topology inference.
///
/// Each flush feeds the transaction announcement stream — unless the peer
/// declined transaction relay — and the peer's `get-mempool` subscription.
/// The task ends when the connection closes.
pub(super) async fn trickle_transaction_announcements(shared: Arc<SharedConnection>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(trickle_delay()) => {}
            _ = shared.quic.closed() => return,
        }

        let batch: Vec<UnminedTxId> = shared
            .pending_tx_announcements
            .with(|pending| pending.drain(..).collect());
        if batch.is_empty() {
            continue;
        }

        // The clone is only paid for when a `get-mempool` subscription is
        // actually listening.
        if shared.mempool_updates.receiver_count() > 0 {
            let _ = shared.mempool_updates.send(batch.clone());
        }

        // A node must not open a transaction announcement stream to a peer
        // whose init record had relay = 0.
        if shared.remote_init.relay {
            for id in batch {
                let mut payload = Vec::with_capacity(65);
                if TransactionReference::from(id).encode(&mut payload).is_ok() {
                    shared.enqueue_announcement(StreamType::TransactionAnnouncements, payload);
                }
            }
        }
    }
}

/// Samples the random trickle delay: exponentially distributed with mean
/// [`TX_TRICKLE_MEAN_INTERVAL`], capped at four times the mean.
fn trickle_delay() -> std::time::Duration {
    let uniform: f64 = rand::random();
    // ln(0) is -inf; the epsilon makes it hit the cap instead.
    let exponential = -(uniform.max(f64::EPSILON)).ln();
    TX_TRICKLE_MEAN_INTERVAL.mul_f64(exponential.min(4.0))
}

/// Fetches `hash` from the local inbound service and queues a block
/// announcement for it.
///
/// Block announcements carry the block header (or a compact block), so the
/// header is fetched from the local node; the internal advertise requests
/// only carry the hash.
async fn announce_block<S>(shared: Arc<SharedConnection>, inbound_service: S, hash: block::Hash)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let response = inbound_service
        .oneshot(Request::BlocksByHash(std::iter::once(hash).collect()))
        .await;

    let block = match response {
        Ok(Response::Blocks(mut blocks)) => match blocks.pop() {
            Some(InventoryResponse::Available((block, _))) => block,
            _ => {
                debug!(?hash, "skipping announcement of block not found locally");
                return;
            }
        },
        other => {
            debug!(?hash, ?other, "skipping block announcement");
            return;
        }
    };

    match build_block_announcement(&block, &shared.remote_init, rand::random()) {
        BlockAnnouncementRecord::Compact { payload, nonce } => {
            shared.record_sent_block(block, Some(nonce));
            shared.enqueue_announcement(StreamType::BlockAnnouncements, payload);
        }
        BlockAnnouncementRecord::Header { payload } => {
            shared.enqueue_announcement(StreamType::BlockAnnouncements, payload);
        }
    }
}

/// A block announcement record payload, built for one remote peer.
pub(super) enum BlockAnnouncementRecord {
    /// A compact block announcement (`kind = 0x01`), with the nonce its
    /// short transaction IDs were computed with.
    Compact { payload: Vec<u8>, nonce: u64 },

    /// A header announcement (`kind = 0x00`).
    Header { payload: Vec<u8> },
}

/// Builds the block announcement record for `block`: a compact block if the
/// peer requested high-bandwidth announcements in its `init` record, and a
/// header announcement otherwise.
///
/// A header announcement is also substituted when the block cannot be
/// encoded as a compact block, or its compact block encoding would exceed
/// the record payload limit.
pub(super) fn build_block_announcement(
    block: &Block,
    remote_init: &InitRecord,
    nonce: u64,
) -> BlockAnnouncementRecord {
    if remote_init.announce {
        // The coinbase transaction must be prefilled, and is; predicting
        // other transactions the peer is missing is left to the peer's
        // `get-tx` follow-up.
        if let Ok(compact) = CompactBlock::from_block(block, nonce, remote_init.full_ids, &[]) {
            let mut payload = Vec::with_capacity(2048);
            payload.push(BLOCK_ANNOUNCEMENT_KIND_COMPACT);
            if compact.encode(&mut payload).is_ok() && payload.len() <= MAX_RECORD_PAYLOAD_LEN {
                return BlockAnnouncementRecord::Compact {
                    payload,
                    nonce: compact.nonce,
                };
            }
        }
    }

    let mut payload = Vec::with_capacity(2048);
    payload.push(BLOCK_ANNOUNCEMENT_KIND_HEADER);
    block
        .header
        .zcash_serialize(&mut payload)
        .expect("serializing a header to a Vec never fails");
    BlockAnnouncementRecord::Header { payload }
}

/// The short transaction IDs of one sent block: each ID that identifies
/// exactly one transaction maps to it, and IDs on which the block's
/// transactions collide map to `None`, since an ambiguous match is answered
/// not-found.
type SentBlockShortIds = std::collections::HashMap<ShortTxId, Option<Arc<Transaction>>>;

/// Returns whether `block`'s transactions match its header's merkle root.
///
/// Checkable by hashing alone, so a mismatch is provable misbehavior or a
/// failed reconstruction, never a chain view difference.
fn block_matches_merkle_root(block: &Block) -> bool {
    let merkle_root: merkle::Root = block.transactions.iter().map(|tx| tx.hash()).collect();

    merkle_root == block.header.merkle_root
}

/// Checks `header`'s context-free proof of work: its Equihash solution, and
/// its hash against its own difficulty threshold.
///
/// Whether the threshold itself is correct is contextual, and is checked
/// when the block is verified; this check bounds the work a forged
/// announcement can demand, and provably attributes an invalid announcement
/// to its sender.
fn header_pow_is_valid(header: &block::Header) -> bool {
    let Some(threshold) = header.difficulty_threshold.to_expanded() else {
        return false;
    };

    header.hash() <= threshold && header.solution.check(header).is_ok()
}

/// Reconstructs the block of a compact block announcement, and hands it to
/// the inbound service as an advertised block.
///
/// The reconstructed block is cached on the connection, so the local gossip
/// downloader's follow-up [`Request::BlocksByHash`] is answered without a
/// wire round-trip. When reconstruction fails, the full block is requested
/// from the announcing peer instead. Either way, the advertiser is withheld
/// from the gossip pipeline: a high-bandwidth announcement may precede the
/// announcer's own full validation, so a block that fails validation is not
/// penalized.
async fn reconstruct_compact_block<S>(
    shared: Arc<SharedConnection>,
    inbound_service: S,
    compact: CompactBlock,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let hash = compact.header.hash();

    let match_service = inbound_service.clone();
    let block = tokio::time::timeout(constants::REQUEST_TIMEOUT, async {
        match match_compact_block(&shared, match_service, &compact).await {
            Some(block) => Some(block),
            // Reconstruction failed: fall back to requesting the full
            // block, without penalizing the peer.
            None => fetch_announced_block(&shared, hash).await,
        }
    })
    .await
    .ok()
    .flatten();

    match block {
        Some(block) => {
            shared
                .reconstructed_blocks
                .with(|cache| insert_bounded(cache, hash, block, RECONSTRUCTED_BLOCK_CACHE_LIMIT));
        }
        None => debug!(?hash, "an announced compact block could not be obtained"),
    }

    let request = Request::AdvertiseBlock(hash, None);
    let _ = call_inbound_service(inbound_service, request).await;
}

/// Attempts to reconstruct the block of a compact block announcement from
/// transactions this node already holds, requesting the unmatched
/// transactions from the announcing peer with `get-tx`.
///
/// Returns `None` when reconstruction fails: a prefilled index is out of
/// range, a requested transaction was answered not-found, or the assembled
/// transactions do not match the header's merkle root (as happens when a
/// short ID collision matched the wrong held transaction).
async fn match_compact_block<S>(
    shared: &Arc<SharedConnection>,
    inbound_service: S,
    compact: &CompactBlock,
) -> Option<Arc<Block>>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let block_hash = compact.header.hash();
    let id_count = match &compact.ids {
        CompactBlockIds::Short(ids) => ids.len(),
        CompactBlockIds::Full(ids) => ids.len(),
    };
    let total = id_count + compact.prefilled.len();

    // Place the prefilled transactions at their absolute indexes; the
    // remaining positions take the identified transactions, in order.
    let mut slots: Vec<Option<Arc<Transaction>>> = vec![None; total];
    for prefilled in &compact.prefilled {
        let slot = slots.get_mut(usize::try_from(prefilled.index).ok()?)?;
        *slot = Some(prefilled.tx.clone());
    }
    let id_positions: Vec<usize> = (0..total).filter(|&index| slots[index].is_none()).collect();

    // Match each identified transaction against the mempool, producing the
    // held candidate to fetch locally, and the reference that requests the
    // transaction from the peer if the candidate falls through.
    let mempool_ids =
        match call_inbound_service(inbound_service.clone(), Request::MempoolTransactionIds).await {
            Ok(Response::TransactionIds(ids)) => ids,
            _ => Vec::new(),
        };

    let id_refs: Vec<(Option<UnminedTxId>, TransactionReference)> = match &compact.ids {
        CompactBlockIds::Short(short_ids) => {
            let header_bytes = compact
                .header
                .zcash_serialize_to_vec()
                .expect("serializing a header to a Vec never fails");
            let (k0, k1) = short_id_keys(&header_bytes, compact.nonce);

            // Held transactions colliding on a short ID map to `None`: the
            // match is ambiguous, so the transaction is requested instead.
            let mut held: HashMap<ShortTxId, Option<UnminedTxId>> =
                HashMap::with_capacity(mempool_ids.len());
            for id in &mempool_ids {
                held.entry(short_transaction_id(k0, k1, id))
                    .and_modify(|matched| *matched = None)
                    .or_insert(Some(*id));
            }

            short_ids
                .iter()
                .map(|short_id| {
                    (
                        held.get(short_id).copied().flatten(),
                        TransactionReference::ShortId {
                            block_hash,
                            short_id: *short_id,
                        },
                    )
                })
                .collect()
        }
        CompactBlockIds::Full(full_ids) => {
            let held: HashMap<[u8; 64], UnminedTxId> = mempool_ids
                .iter()
                .map(|id| (full_transaction_id(id).as_bytes(), *id))
                .collect();

            full_ids
                .iter()
                .map(|full_id| {
                    (
                        held.get(&full_id.as_bytes()).copied(),
                        txref_from_full_id(full_id),
                    )
                })
                .collect()
        }
    };

    // Fetch the held candidates from the mempool.
    let held_ids: HashSet<UnminedTxId> = id_refs.iter().filter_map(|(id, _)| *id).collect();
    let mut held_txs: HashMap<UnminedTxId, Arc<Transaction>> =
        HashMap::with_capacity(held_ids.len());
    if !held_ids.is_empty() {
        if let Ok(Response::Transactions(transactions)) =
            call_inbound_service(inbound_service, Request::TransactionsById(held_ids)).await
        {
            for entry in transactions {
                if let InventoryResponse::Available((tx, _)) = entry {
                    held_txs.insert(tx.id, tx.transaction);
                }
            }
        }
    }

    // Fill the identified positions, and request everything unmatched — or
    // matched but gone from the mempool since — from the peer.
    let mut wire_refs: Vec<TransactionReference> = Vec::new();
    let mut wire_positions: Vec<usize> = Vec::new();
    for (&position, (held_id, txref)) in id_positions.iter().zip(&id_refs) {
        match held_id.and_then(|id| held_txs.get(&id).cloned()) {
            Some(tx) => slots[position] = Some(tx),
            None => {
                wire_refs.push(*txref);
                wire_positions.push(position);
            }
        }
    }

    if !wire_refs.is_empty() {
        let mut recv = send_wire_request(shared, WireRequest::GetTx { refs: wire_refs })
            .await
            .ok()?;
        for position in wire_positions {
            match TxResponseEntry::read(&mut recv).await.ok()? {
                TxResponseEntry::Found(tx) => slots[position] = Some(tx),
                TxResponseEntry::NotFound => return None,
            }
        }
        record::expect_end_of_stream(&mut recv).await.ok()?;
    }

    let transactions: Vec<Arc<Transaction>> = slots.into_iter().collect::<Option<Vec<_>>>()?;
    let block = Block {
        header: compact.header.clone(),
        transactions,
    };

    // A wrong short ID match assembles the wrong block. The merkle root
    // check detects that here, so this node falls back to the full block
    // instead of relaying a block that fails validation.
    if !block_matches_merkle_root(&block) {
        return None;
    }

    Some(Arc::new(block))
}

/// Builds the `get-tx` reference requesting an unmatched full transaction
/// ID: a `TXID` reference for pre-v5 transactions, identified by the
/// ZIP 244 auth digest placeholder, and a `WTXID` reference otherwise.
fn txref_from_full_id(full_id: &WtxId) -> TransactionReference {
    if full_id.auth_digest == merkle::AUTH_DIGEST_PLACEHOLDER {
        TransactionReference::Txid(full_id.id)
    } else {
        TransactionReference::Wtxid(*full_id)
    }
}

/// Maintains this node's `get-mempool` subscription to the peer: mirrors
/// the peer's mempool into the connection's cache, which answers internal
/// [`Request::MempoolTransactionIds`], and forwards newly referenced
/// transactions to the inbound service as advertisements.
///
/// The subscription ends when the peer finishes or refuses it, or the
/// connection closes; the mirrored cache keeps its contents either way.
pub(super) async fn maintain_mempool_subscription<S>(
    shared: Arc<SharedConnection>,
    inbound_service: S,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let mut recv = match send_wire_request(&shared, WireRequest::GetMempool).await {
        Ok(recv) => recv,
        Err(_) => return,
    };

    loop {
        // Subscription records arrive whenever the peer's mempool changes,
        // so an idle stream is normal, and there is no read timeout.
        let payload = match record::read_record(&mut recv).await {
            Ok(Some(payload)) => payload,
            // The peer finished the subscription, for example on shutdown.
            Ok(None) => return,
            Err(error) => {
                // A reset carries the peer's refusal or cancellation; only
                // malformed response data fails the connection.
                if !is_stream_failure(&error) {
                    shared.fail_wire_error(&error);
                }
                return;
            }
        };

        let mut reader = payload.as_slice();
        let response = match MempoolResponse::read(&mut reader).await {
            Ok(response) if reader.is_empty() => response,
            Ok(_) => {
                shared.fail_protocol("trailing bytes in a get-mempool record");
                return;
            }
            Err(error) => {
                shared.fail_wire_error(&error);
                return;
            }
        };

        let ids: Vec<UnminedTxId> = response
            .0
            .into_iter()
            .map(|txref| {
                txref
                    .try_into()
                    .expect("read rejects SHORTID references in get-mempool records")
            })
            .collect();
        if ids.is_empty() {
            continue;
        }

        // Mirror the references into the cache, evicting the oldest half
        // beyond the bound.
        shared.remote_mempool.with(|cache| {
            cache.extend(ids.iter().copied());
            let len = cache.len();
            if len > MAX_MEMPOOL_RESPONSE_REFS {
                let recent = cache.split_off(len / 2);
                *cache = recent;
            }
        });

        // Forward the references as an advertisement: the mempool dedups
        // known transactions and downloads the rest with `get-tx`.
        if let Some(addr) = shared.transient_addr {
            let hashes: Vec<InventoryHash> =
                ids.iter().map(|id| InventoryHash::from(*id)).collect();
            if let Some(change) = InventoryChange::new_available_multi(hashes.iter(), addr) {
                let _ = shared.inv_collector.send(change);
            }
        }
        let request =
            Request::AdvertiseTransactionIds(ids.into_iter().collect(), shared.transient_addr);
        let _ = call_inbound_service(inbound_service.clone(), request).await;
    }
}

/// Requests the full block for `hash` from the peer, after a compact block
/// announcement that could not be reconstructed.
async fn fetch_announced_block(shared: &SharedConnection, hash: block::Hash) -> Option<Arc<Block>> {
    let mut recv = send_wire_request(shared, WireRequest::GetBlocks { hashes: vec![hash] })
        .await
        .ok()?;
    let entry = BlockResponseEntry::read(&mut recv).await.ok()?;
    record::expect_end_of_stream(&mut recv).await.ok()?;

    match entry {
        BlockResponseEntry::Full(block) if block.hash() == hash => Some(block),
        _ => None,
    }
}

/// Builds the short transaction ID table of a block, using the nonce of the
/// compact block sent for it.
fn short_ids_of(block: &Block, nonce: u64) -> Option<SentBlockShortIds> {
    let header_bytes = block
        .header
        .zcash_serialize_to_vec()
        .expect("serializing a header to a Vec never fails");
    let (k0, k1) = short_id_keys(&header_bytes, nonce);

    let mut table = SentBlockShortIds::with_capacity(block.transactions.len());
    for tx in &block.transactions {
        let short_id = short_transaction_id(k0, k1, &UnminedTxId::from(tx.as_ref()));
        table
            .entry(short_id)
            .and_modify(|matched| *matched = None)
            .or_insert_with(|| Some(tx.clone()));
    }

    Some(table)
}

/// Spawns a task that performs `request` on its own request stream and sends
/// the result to `tx`.
fn spawn_outbound_request(
    shared: &Arc<SharedConnection>,
    request: Request,
    tx: MustUseClientResponseSender,
    span: tracing::Span,
) {
    let shared = shared.clone();
    tokio::spawn(
        async move {
            let command = request.command();
            let result = tokio::time::timeout(
                constants::REQUEST_TIMEOUT,
                drive_outbound_request(&shared, request),
            )
            .await
            .unwrap_or(Err(OutboundError::Timeout));

            let result = match result {
                Ok(response) => {
                    shared.record_response();
                    Ok(response)
                }
                Err(error) => Err(error.into_shared_error(&shared, command)),
            };

            let _ = tx.send(result);
        }
        .instrument(span),
    );
}

/// An error performing an outbound request.
enum OutboundError {
    /// The response did not arrive within the request timeout.
    Timeout,

    /// The peer answered every requested item with a not-found result.
    NotFound(Vec<InventoryHash>),

    /// A wire or transport error.
    Wire(WireError),

    /// A local error that leaves the connection usable.
    Local(PeerError),
}

impl From<WireError> for OutboundError {
    fn from(error: WireError) -> Self {
        OutboundError::Wire(error)
    }
}

impl OutboundError {
    /// Converts this error to the [`SharedPeerError`] reported to the
    /// caller, failing the connection for connection-level errors.
    ///
    /// `command` is the command name of the failed request, for logging.
    fn into_shared_error(
        self,
        shared: &SharedConnection,
        command: &'static str,
    ) -> SharedPeerError {
        match self {
            OutboundError::Timeout => {
                shared.record_request_timeout();
                PeerError::ConnectionReceiveTimeout.into()
            }
            // A local error: the peer is not at fault, so the connection
            // stays open.
            OutboundError::Local(error) => error.into(),
            // The peer answered, so it is not unresponsive: a not-found
            // response is a valid answer.
            OutboundError::NotFound(hashes) => {
                shared.record_response();
                PeerError::NotFoundResponse(hashes).into()
            }
            // This node could not build a valid request: the peer is not at
            // fault, so the connection stays open.
            OutboundError::Wire(WireError::Local(reason)) => {
                warn!(request = %command, %reason, "could not encode a v2 request");
                PeerError::V2Internal(reason).into()
            }

            // The peer violated a limit that is scored rather than
            // disconnected on: the request fails, and the peer is banned once
            // its accumulated score reaches the threshold.
            OutboundError::Wire(WireError::Misbehavior { points, reason }) => {
                shared.report_misbehavior(points, &reason);
                PeerError::V2Protocol(reason).into()
            }
            OutboundError::Wire(error) => match stream_reset_code(&error) {
                // The responder declined the request by resetting the
                // stream; the connection remains usable.
                Some(ErrorCode::Refused) => {
                    PeerError::V2Protocol(format!("peer refused {command}")).into()
                }
                _ => match error.connection_error_code() {
                    // The peer sent an invalid response: this is a
                    // connection error of the indicated type.
                    Some(code) if !is_stream_failure(&error) => {
                        let message = error.to_string();
                        shared.fail(code, PeerError::V2Protocol(message.clone()));
                        PeerError::V2Protocol(message).into()
                    }
                    _ => {
                        warn!(request = %command, %error, "outbound v2 request failed");
                        PeerError::V2Protocol(error.to_string()).into()
                    }
                },
            },
        }
    }
}

/// Returns the application error code if `error` is a stream reset by the
/// peer.
fn stream_reset_code(error: &WireError) -> Option<ErrorCode> {
    if let WireError::Io(io_error) = error {
        let mut source: Option<&(dyn std::error::Error + 'static)> =
            io_error.get_ref().map(|error| error as _);
        while let Some(error) = source {
            if let Some(quinn::ReadError::Reset(code)) = error.downcast_ref::<quinn::ReadError>() {
                return Some(ErrorCode::from_wire(code.into_inner()));
            }
            source = error.source();
        }
    }

    None
}

/// Returns true if `error` reading a stream was caused by a stream reset, a
/// transport failure, or a stalled peer, rather than by invalid data sent by
/// the peer.
///
/// These errors abandon the stream, but leave the connection usable.
fn is_stream_failure(error: &WireError) -> bool {
    matches!(error, WireError::Io(_) | WireError::Timeout(_))
}

/// Performs `request` on a new request stream and decodes the response.
async fn drive_outbound_request(
    shared: &SharedConnection,
    request: Request,
) -> Result<Response, OutboundError> {
    let transient_addr = shared.transient_addr;

    match request {
        Request::Peers => {
            let mut recv = send_wire_request(shared, WireRequest::GetAddr).await?;
            let addrs = AddrResponse::read(&mut recv).await?;
            record::expect_end_of_stream(&mut recv).await?;

            // # Security
            //
            // Merge the response into the address cache, and answer with a
            // bounded random sample, so a single peer cannot control the
            // address book by the size or order of its response, like the
            // legacy connection.
            let response_addrs = shared.cached_addrs.with(|cached_addrs| {
                update_addr_cache(cached_addrs, &addrs.0, constants::PEER_ADDR_RESPONSE_LIMIT)
            });

            Ok(Response::Peers(response_addrs))
        }

        Request::MempoolTransactionIds => {
            // Answered from the mempool subscription's mirror: only one
            // concurrent `get-mempool` stream per peer is allowed, and the
            // connection's subscription task holds it.
            let ids: Vec<UnminedTxId> = shared.remote_mempool.read().iter().copied().collect();
            Ok(Response::TransactionIds(ids))
        }

        Request::FindHeaders { known_blocks, stop } => {
            let (headers, _hashes) = get_headers(shared, known_blocks, stop).await?;
            Ok(Response::BlockHeaders(headers.0))
        }

        // The v2 protocol is headers-first only: the legacy inventory walk
        // is bridged onto `get-headers` by hashing the returned headers.
        Request::FindBlocks { known_blocks, stop } => {
            let (_headers, hashes) = get_headers(shared, known_blocks, stop).await?;
            Ok(Response::BlockHashes(hashes))
        }

        Request::BlocksByHash(hashes) => {
            let hashes: Vec<block::Hash> = hashes.into_iter().collect();

            // Blocks reconstructed from this peer's compact block
            // announcements answer without a wire request: the gossip
            // downloader fetches an announced block right back from the
            // announcing connection.
            let reconstructed: Option<Vec<Arc<Block>>> = {
                let cache = shared.reconstructed_blocks.read();
                hashes.iter().map(|hash| cache.get(hash).cloned()).collect()
            };
            if let Some(blocks) = reconstructed {
                return Ok(Response::Blocks(
                    blocks
                        .into_iter()
                        .map(|block| InventoryResponse::Available((block, transient_addr)))
                        .collect(),
                ));
            }

            let recv = send_wire_request(
                shared,
                WireRequest::GetBlocks {
                    hashes: hashes.clone(),
                },
            )
            .await?;

            let blocks = read_matched_entries(
                recv,
                &hashes,
                transient_addr,
                "get-blocks",
                "block",
                async |recv| match BlockResponseEntry::read(recv).await? {
                    BlockResponseEntry::Full(block) => Ok(Some((block.hash(), block))),
                    BlockResponseEntry::NotFound => Ok(None),
                },
            )
            .await?;

            Ok(Response::Blocks(blocks))
        }

        Request::TransactionsById(ids) => {
            let ids: Vec<UnminedTxId> = ids.into_iter().collect();
            let refs = ids
                .iter()
                .map(|id| TransactionReference::from(*id))
                .collect();

            let recv = send_wire_request(shared, WireRequest::GetTx { refs }).await?;

            let transactions = read_matched_entries(
                recv,
                &ids,
                transient_addr,
                "get-tx",
                "transaction",
                async |recv| match TxResponseEntry::read(recv).await? {
                    TxResponseEntry::Found(transaction) => {
                        let transaction = UnminedTx::from(transaction);
                        Ok(Some((transaction.id, transaction)))
                    }
                    TxResponseEntry::NotFound => Ok(None),
                },
            )
            .await?;

            Ok(Response::Transactions(transactions))
        }

        Request::Ping(_)
        | Request::PushTransaction(_, _)
        | Request::AdvertiseTransactionIds(_, _)
        | Request::AdvertiseBlock(_, _)
        | Request::AdvertiseBlockToAll(_) => {
            unreachable!("locally-handled requests are dispatched before this point")
        }

        Request::BlockRange {
            final_hash,
            count,
            max_bytes,
        } => {
            let mut recv = send_wire_request(
                shared,
                WireRequest::GetBlockRange {
                    final_hash,
                    count,
                    max_bytes,
                },
            )
            .await?;

            match record::read_u8(&mut recv).await? {
                RESULT_OBJECT => {}
                RESULT_NOT_FOUND => {
                    record::expect_end_of_stream(&mut recv).await?;
                    return Ok(Response::Blocks(vec![InventoryResponse::Missing(
                        final_hash,
                    )]));
                }
                unknown => {
                    return Err(OutboundError::Wire(WireError::Protocol(format!(
                        "unrecognized get-block-range result value {unknown:#04x}",
                    ))));
                }
            }

            // Every block is verified on arrival, by hashing alone: the
            // first block must hash to the anchor, each subsequent block
            // must be the parent of the previous one, and each block's
            // transactions must match its header's merkle root. A violation
            // is a connection error of type `PROTOCOL_ERROR`, and exceeding
            // either request bound is `FLOOD`: both are checkable without
            // any chain state, so blame is exact.
            let mut blocks = Vec::new();
            let mut expected_hash = final_hash;
            let mut delivered_bytes: u64 = 0;

            while let Some(payload) = record::read_record(&mut recv).await? {
                if blocks.len() as u64 >= count {
                    return Err(OutboundError::Wire(WireError::Flood(
                        "get-block-range delivered more blocks than requested".to_string(),
                    )));
                }
                if !blocks.is_empty()
                    && delivered_bytes.saturating_add(payload.len() as u64) > max_bytes
                {
                    return Err(OutboundError::Wire(WireError::Flood(
                        "get-block-range delivered blocks past the requested byte bound"
                            .to_string(),
                    )));
                }

                let block = Arc::new(
                    Block::zcash_deserialize(payload.as_slice()).map_err(WireError::from)?,
                );
                if block.hash() != expected_hash {
                    return Err(OutboundError::Wire(WireError::Protocol(
                        "get-block-range block does not extend the verified chain".to_string(),
                    )));
                }
                if !block_matches_merkle_root(&block) {
                    return Err(OutboundError::Wire(WireError::Protocol(
                        "get-block-range block does not match its merkle root".to_string(),
                    )));
                }

                delivered_bytes = delivered_bytes.saturating_add(payload.len() as u64);
                expected_hash = block.header.previous_block_hash;
                blocks.push(InventoryResponse::Available((block, transient_addr)));
            }

            Ok(Response::Blocks(blocks))
        }

        // These are answered by the local inbound service, never by a
        // peer: a mis-routed request is a local wiring mistake, so the
        // caller is told rather than the connection task panicking.
        request @ (Request::SyncHashes { .. } | Request::TreeRoots { .. }) => Err(
            OutboundError::Local(PeerError::LocalOnlyRequest(request.command())),
        ),
    }
}

/// Reads one result-tagged response entry per requested key, in request
/// order, and checks that the stream ends after the last entry.
///
/// `read_entry` reads a single entry, returning the item and its identifying
/// key, or `None` for a not-found entry.
///
/// # Security
///
/// Each returned item's key must match the requested key, so a peer cannot
/// substitute an unrequested item for a requested one. If every entry is
/// not-found, a retryable [`OutboundError::NotFound`] is reported, like the
/// legacy connection.
async fn read_matched_entries<K, T>(
    mut recv: tokio::io::BufReader<quinn::RecvStream>,
    keys: &[K],
    transient_addr: Option<PeerSocketAddr>,
    request_kind: &'static str,
    item_kind: &'static str,
    mut read_entry: impl AsyncFnMut(
        &mut tokio::io::BufReader<quinn::RecvStream>,
    ) -> Result<Option<(K, T)>, WireError>,
) -> Result<Vec<InventoryResponse<(T, Option<PeerSocketAddr>), K>>, OutboundError>
where
    K: Copy + Eq + std::fmt::Display + Into<InventoryHash>,
{
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        match read_entry(&mut recv).await? {
            Some((entry_key, item)) => {
                // # Security
                //
                // Check that the response matches the request before using
                // it.
                if entry_key != *key {
                    return Err(WireError::Protocol(format!(
                        "peer answered a {request_kind} entry for {key} \
                         with a different {item_kind}",
                    ))
                    .into());
                }
                entries.push(InventoryResponse::Available((item, transient_addr)));
            }
            None => entries.push(InventoryResponse::Missing(*key)),
        }
    }
    record::expect_end_of_stream(&mut recv).await?;

    // If the peer has none of the requested items, report a retryable
    // not-found error, like the legacy connection.
    if entries
        .iter()
        .all(|entry| matches!(entry, InventoryResponse::Missing(_)))
    {
        return Err(OutboundError::NotFound(
            keys.iter().copied().map(Into::into).collect(),
        ));
    }

    Ok(entries)
}

/// Sends a `get-headers` request and reads and validates the response,
/// returning it along with the hash of each returned header.
async fn get_headers(
    shared: &SharedConnection,
    known_blocks: Vec<block::Hash>,
    stop: Option<block::Hash>,
) -> Result<(HeadersResponse, Vec<block::Hash>), OutboundError> {
    let mut recv = send_wire_request(
        shared,
        WireRequest::GetHeaders {
            known_blocks,
            stop,
            // The internal protocol has no request for headers with
            // transaction IDs: the transaction-ID form is used by the
            // compact block reconstruction path.
            tx_ids: false,
        },
    )
    .await?;
    let headers = HeadersResponse::read(&mut recv).await?;
    record::expect_end_of_stream(&mut recv).await?;

    let hashes = headers.check_contiguous()?;

    Ok((headers, hashes))
}

/// Opens a request stream, writes `request`, and finishes the sending
/// direction, returning the buffered receiving half.
async fn send_wire_request(
    shared: &SharedConnection,
    request: WireRequest,
) -> Result<tokio::io::BufReader<quinn::RecvStream>, WireError> {
    let (mut send, recv) = shared
        .quic
        .open_bi()
        .await
        .map_err(|error| WireError::Io(std::io::Error::other(error)))?;

    let mut bytes = vec![request.stream_type().byte()];
    request.encode(&mut bytes)?;

    send.write_all(&bytes)
        .await
        .map_err(|error| WireError::Io(std::io::Error::other(error)))?;
    send.finish()
        .map_err(|error| WireError::Io(std::io::Error::other(error)))?;

    Ok(tokio::io::BufReader::with_capacity(
        RESPONSE_STREAM_READ_BUFFER_SIZE,
        recv,
    ))
}

/// Serves one inbound request stream from the remote peer.
///
/// Reading the request, and serving any single-response request, is bounded
/// by [`INBOUND_STREAM_TIMEOUT`]: a peer that opens a stream and then
/// stalls, or that stops reading its response, would otherwise pin this task
/// and its buffers for the life of the connection. A `get-mempool`
/// subscription is exempt: its response is open-ended by design, and it is
/// bounded instead by the requester cancelling it or the connection closing.
async fn serve_inbound_request_stream<S>(
    shared: Arc<SharedConnection>,
    inbound_service: S,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let read = read_inbound_request(&shared, &mut send, &mut recv);
    let request = match tokio::time::timeout(INBOUND_STREAM_TIMEOUT, read).await {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(_elapsed) => {
            debug!("abandoning a stalled inbound v2 request stream");
            let _ = recv.stop(ErrorCode::Cancelled.into());
            let _ = send.reset(ErrorCode::Cancelled.into());
            return;
        }
    };

    let result = if matches!(request, WireRequest::GetMempool) {
        serve_mempool_subscription(&shared, inbound_service, &mut send).await
    } else {
        let serve = serve_request(&shared, inbound_service, request, &mut send);
        match tokio::time::timeout(INBOUND_STREAM_TIMEOUT, serve).await {
            Ok(result) => result,
            Err(_elapsed) => {
                debug!("abandoning a stalled inbound v2 request stream");
                let _ = send.reset(ErrorCode::Cancelled.into());
                return;
            }
        }
    };

    match result {
        Ok(()) => {
            let _ = send.finish();
        }
        Err(ServeError::Refused) => {
            let _ = send.reset(ErrorCode::Refused.into());
        }
        Err(ServeError::Encode(error)) => {
            debug!(%error, "internal error encoding a v2 response");
            let _ = send.reset(ErrorCode::InternalError.into());
        }
        // The requester cancelled the request or the connection closed.
        Err(ServeError::Write) => {}
    }
}

/// Reads and decodes the request of one inbound request stream, through the
/// end of its receive direction.
///
/// Returns `None` when the stream was refused or failed; protocol
/// violations fail the connection.
async fn read_inbound_request(
    shared: &Arc<SharedConnection>,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
) -> Option<WireRequest> {
    let type_byte = read_stream_type_or_fail(shared, recv).await?;

    let stream_type = match StreamType::from_byte(type_byte) {
        Some(stream_type) if stream_type.is_request() => stream_type,
        Some(StreamType::Handshake) => {
            // Only the initiator opens the handshake stream, and a
            // connection has exactly one.
            shared.fail_protocol("unexpected extra handshake stream");
            return None;
        }
        Some(_announcement_type) => {
            shared.fail_protocol("announcement stream type on a bidirectional stream");
            return None;
        }
        // Refuse unrecognized stream types without penalizing the peer, so
        // future stream types can be deployed without version gating.
        None => {
            let _ = recv.stop(ErrorCode::UnsupportedStreamType.into());
            let _ = send.reset(ErrorCode::UnsupportedStreamType.into());
            return None;
        }
    };

    let mut recv =
        tokio::io::BufReader::with_capacity(INBOUND_REQUEST_STREAM_READ_BUFFER_SIZE, recv);
    let request = match WireRequest::read(stream_type, &mut recv).await {
        Ok(request) => request,
        Err(error) => {
            handle_inbound_wire_error(shared, error);
            return None;
        }
    };
    if let Err(error) = record::expect_end_of_stream(&mut recv).await {
        handle_inbound_wire_error(shared, error);
        return None;
    }

    // To impede fingerprinting, only answer address requests on inbound
    // connections.
    if matches!(request, WireRequest::GetAddr) && !shared.is_inbound {
        let _ = send.reset(ErrorCode::Refused.into());
        return None;
    }

    Some(request)
}

/// Serves a `get-mempool` subscription: one or more records referencing the
/// whole mempool (the snapshot), then a record per batch of transactions
/// accepted into the mempool, until the requester cancels the subscription
/// or the connection ends.
async fn serve_mempool_subscription<S>(
    shared: &Arc<SharedConnection>,
    inbound_service: S,
    send: &mut quinn::SendStream,
) -> Result<(), ServeError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    // A peer must not open more than one concurrent subscription.
    if shared.mempool_subscribed.swap(true, Ordering::AcqRel) {
        shared.fail_protocol("second concurrent get-mempool subscription");
        return Err(ServeError::Write);
    }

    // Clears the subscription slot when serving ends for any reason, so the
    // peer can subscribe again sequentially.
    struct SubscriptionGuard<'a>(&'a SharedConnection);
    impl Drop for SubscriptionGuard<'_> {
        fn drop(&mut self) {
            self.0.mempool_subscribed.store(false, Ordering::Release);
        }
    }
    let _guard = SubscriptionGuard(shared);

    // Subscribe to updates before reading the snapshot, so transactions
    // accepted while the snapshot is sent are not missed. The overlap can
    // only duplicate references, which the requester tolerates.
    let mut updates = shared.mempool_updates.subscribe();

    let snapshot = mempool_transaction_ids(inbound_service.clone()).await?;
    write_mempool_records(send, &snapshot).await?;

    loop {
        let update = tokio::select! {
            update = updates.recv() => update,
            // The requester cancelled the subscription. Noticing here, and
            // not on the next write, releases the subscription slot while
            // the mempool is quiet, so the peer can subscribe again.
            _ = send.stopped() => return Ok(()),
        };

        let batch = match update {
            Ok(ids) => ids,
            // The channel lagged: some accepted transactions were lost, so
            // re-send a whole snapshot. Duplicates are tolerated.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                mempool_transaction_ids(inbound_service.clone()).await?
            }
            // The connection is shutting down.
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        };

        if !batch.is_empty() {
            write_mempool_records(send, &batch).await?;
        }
    }
}

/// Fetches the local mempool's transaction IDs for a `get-mempool`
/// subscription record.
async fn mempool_transaction_ids<S>(inbound_service: S) -> Result<Vec<UnminedTxId>, ServeError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let response = call_inbound_service(inbound_service, Request::MempoolTransactionIds).await?;
    Ok(match response {
        Response::TransactionIds(ids) => ids,
        _ => Vec::new(),
    })
}

/// Writes `ids` as one or more `get-mempool` response records, splitting so
/// that every record stays within the payload limit.
///
/// An empty `ids` writes a single `count = 0` record: the snapshot of an
/// empty mempool.
async fn write_mempool_records(
    send: &mut quinn::SendStream,
    ids: &[UnminedTxId],
) -> Result<(), ServeError> {
    let mut bytes = Vec::new();

    if ids.is_empty() {
        let mut payload = Vec::new();
        MempoolResponse(Vec::new())
            .encode(&mut payload)
            .map_err(ServeError::Encode)?;
        record::write_record(&mut bytes, &payload).map_err(ServeError::Encode)?;
        return write_response_bytes(send, &bytes).await;
    }

    for chunk in ids.chunks(MAX_MEMPOOL_RECORD_REFS) {
        let refs = chunk
            .iter()
            .map(|id| TransactionReference::from(*id))
            .collect();

        let mut payload = Vec::with_capacity(chunk.len() * 65 + 9);
        MempoolResponse(refs)
            .encode(&mut payload)
            .map_err(ServeError::Encode)?;

        bytes.clear();
        record::write_record(&mut bytes, &payload).map_err(ServeError::Encode)?;
        write_response_bytes(send, &bytes).await?;
    }

    Ok(())
}

/// An error serving an inbound request.
enum ServeError {
    /// The inbound service declined or failed to serve the request.
    Refused,

    /// The response could not be encoded.
    Encode(WireError),

    /// The response could not be written to the stream.
    ///
    /// The write error itself is not reported: the requester cancelled the
    /// request or the connection is closing.
    Write,
}

impl From<WireError> for ServeError {
    fn from(error: WireError) -> Self {
        ServeError::Encode(error)
    }
}

/// Serves a decoded inbound request, writing the response to `send`.
async fn serve_request<S>(
    shared: &Arc<SharedConnection>,
    inbound_service: S,
    request: WireRequest,
    send: &mut quinn::SendStream,
) -> Result<(), ServeError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    match request {
        WireRequest::GetHeaders {
            known_blocks,
            stop,
            tx_ids,
        } => {
            let response = call_inbound_service(
                inbound_service.clone(),
                Request::FindHeaders { known_blocks, stop },
            )
            .await?;
            let headers = match response {
                Response::BlockHeaders(headers) => headers,
                _ => Vec::new(),
            };

            if !tx_ids {
                let mut bytes = Vec::new();
                HeadersResponse(headers).encode(&mut bytes)?;
                return write_response_bytes(send, &bytes).await;
            }

            // The transaction-ID form additionally identifies each block's
            // transactions: the coinbase transaction in full (it is never
            // otherwise relayed), and every other transaction by its full
            // transaction ID. Headers whose blocks this node cannot supply
            // are answered with `has_txs = 0x00`, and the requester falls
            // back to `get-blocks`.
            let hashes: Vec<block::Hash> =
                headers.iter().map(|header| header.header.hash()).collect();
            let response = call_inbound_service(
                inbound_service,
                Request::BlocksByHash(hashes.iter().copied().collect()),
            )
            .await?;

            let available = available_blocks(response);

            let mut bytes = Vec::new();
            record::write_compact_size(&mut bytes, headers.len() as u64)
                .map_err(ServeError::Encode)?;
            write_response_bytes(send, &bytes).await?;

            for (header, hash) in headers.iter().zip(hashes) {
                bytes.clear();

                let header_bytes = header
                    .header
                    .zcash_serialize_to_vec()
                    .expect("serializing a header to a Vec never fails");
                record::write_record(&mut bytes, &header_bytes).map_err(ServeError::Encode)?;

                match available.get(&hash) {
                    Some(block) => {
                        // The peer can now request this block's transactions
                        // from the IDs below, so they must stay servable.
                        shared.record_sent_block(block.clone(), None);

                        bytes.push(HEADERS_ENTRY_HAS_TXS);

                        let coinbase_bytes = block
                            .transactions
                            .first()
                            .expect("valid blocks have a coinbase transaction")
                            .zcash_serialize_to_vec()
                            .map_err(|err| {
                                ServeError::Encode(WireError::Local(format!(
                                    "unserializable coinbase transaction: {err}"
                                )))
                            })?;
                        record::write_record(&mut bytes, &coinbase_bytes)
                            .map_err(ServeError::Encode)?;

                        let ids: Vec<_> = block
                            .transactions
                            .iter()
                            .skip(1)
                            .map(|tx| full_transaction_id(&UnminedTxId::from(tx.as_ref())))
                            .collect();
                        record::write_compact_size(&mut bytes, ids.len() as u64)
                            .map_err(ServeError::Encode)?;
                        for id in ids {
                            bytes.extend_from_slice(&id.as_bytes());
                        }
                    }
                    None => bytes.push(HEADERS_ENTRY_NO_TXS),
                }

                write_response_bytes(send, &bytes).await?;
            }

            Ok(())
        }

        WireRequest::GetBlocks { hashes } => {
            let requested: HashSet<block::Hash> = hashes.iter().copied().collect();
            let response =
                call_inbound_service(inbound_service, Request::BlocksByHash(requested)).await?;

            let available = available_blocks(response);

            // Reuse one buffer across the entries, so each large entry does
            // not grow a fresh buffer from empty.
            let mut bytes = Vec::new();
            for hash in hashes {
                let entry = match available.get(&hash) {
                    Some(block) => BlockResponseEntry::Full(block.clone()),
                    None => BlockResponseEntry::NotFound,
                };

                bytes.clear();
                entry.encode(&mut bytes)?;
                write_response_bytes(send, &bytes).await?;
            }

            Ok(())
        }

        WireRequest::GetTx { refs } => {
            // Resolve each reference to a transaction source: an ID to look
            // up, a transaction resolved directly from a sent block, or
            // not-found.
            enum TxSource {
                ById(UnminedTxId),
                Direct(Arc<Transaction>),
                NotFound,
            }

            // Short ID tables for the referenced blocks, built at most once
            // per block per request. `None` records a block without a
            // servable compact block, so repeated references to it stay
            // cheap.
            let mut short_id_tables: IndexMap<block::Hash, Option<SentBlockShortIds>> =
                IndexMap::new();

            let mut ids: Vec<TxSource> = Vec::with_capacity(refs.len());
            for txref in &refs {
                match txref {
                    TransactionReference::Txid(_) | TransactionReference::Wtxid(_) => {
                        let id = (*txref)
                            .try_into()
                            .expect("TXID and WTXID references convert to unmined ids");
                        ids.push(TxSource::ById(id))
                    }
                    // Short transaction IDs identify a transaction within
                    // the compact block most recently sent to this peer for
                    // the referenced block. A reference to a block without a
                    // sent compact block, or whose short ID matches no
                    // transaction — or more than one — in the block, is
                    // answered not-found.
                    TransactionReference::ShortId {
                        block_hash,
                        short_id,
                    } => {
                        let table = short_id_tables.entry(*block_hash).or_insert_with(|| {
                            // Take a handle to the block, then build the
                            // table outside the borrow: hashing every
                            // transaction of a block must not block the
                            // announcement and reconstruction paths that
                            // share this cell.
                            let sent = shared
                                .sent_blocks
                                .read()
                                .get(block_hash)
                                .map(|sent| (sent.block.clone(), sent.nonce));

                            sent.and_then(|(block, nonce)| short_ids_of(&block, nonce?))
                        });

                        let source = match table.as_ref().and_then(|table| table.get(short_id)) {
                            Some(Some(tx)) => TxSource::Direct(tx.clone()),
                            _ => TxSource::NotFound,
                        };
                        ids.push(source);
                    }
                }
            }

            let lookup_ids: HashSet<UnminedTxId> = ids
                .iter()
                .filter_map(|source| match source {
                    TxSource::ById(id) => Some(*id),
                    _ => None,
                })
                .collect();

            // Transactions resolved outside the mempool, served as-is.
            let mut direct: IndexMap<UnminedTxId, Arc<Transaction>> = IndexMap::new();

            let mut available: IndexMap<UnminedTxId, UnminedTx> = {
                let pushed = shared.pushed_transactions.read();
                lookup_ids
                    .iter()
                    .filter_map(|id| pushed.get(id).map(|tx| (*id, tx.clone())))
                    .collect()
            };

            let missing_ids: HashSet<UnminedTxId> = lookup_ids
                .iter()
                .filter(|id| !available.contains_key(*id))
                .copied()
                .collect();
            if !missing_ids.is_empty() {
                let response = call_inbound_service(
                    inbound_service,
                    Request::TransactionsById(missing_ids.clone()),
                )
                .await?;
                if let Response::Transactions(transactions) = response {
                    for entry in transactions {
                        if let InventoryResponse::Available((transaction, _)) = entry {
                            available.insert(transaction.id, transaction);
                        }
                    }
                }
            }

            // Recently sent blocks fill remaining misses: their transactions
            // must stay servable to this peer even after leaving the
            // mempool, so reconstruction can complete.
            let mut unresolved: HashSet<UnminedTxId> = missing_ids
                .iter()
                .filter(|id| !available.contains_key(*id))
                .copied()
                .collect();
            if !unresolved.is_empty() {
                // Take handles to the blocks, then search outside the lock.
                let sent: Vec<Arc<Block>> = {
                    let sent = shared.sent_blocks.read();
                    sent.values().map(|entry| entry.block.clone()).collect()
                };

                'blocks: for block in sent {
                    for tx in &block.transactions {
                        let id = UnminedTxId::from(tx.as_ref());
                        if unresolved.remove(&id) {
                            direct.insert(id, tx.clone());
                            if unresolved.is_empty() {
                                break 'blocks;
                            }
                        }
                    }
                }
            }

            // Reuse one buffer across the entries, so each large entry does
            // not grow a fresh buffer from empty.
            let mut bytes = Vec::new();
            for source in ids {
                let entry = match source {
                    TxSource::ById(id) => match available.get(&id) {
                        Some(transaction) => {
                            TxResponseEntry::Found(transaction.transaction.clone())
                        }
                        None => match direct.get(&id) {
                            Some(transaction) => TxResponseEntry::Found(transaction.clone()),
                            None => TxResponseEntry::NotFound,
                        },
                    },
                    TxSource::Direct(transaction) => TxResponseEntry::Found(transaction),
                    TxSource::NotFound => TxResponseEntry::NotFound,
                };

                bytes.clear();
                entry.encode(&mut bytes)?;
                write_response_bytes(send, &bytes).await?;
            }

            Ok(())
        }

        WireRequest::GetAddr => {
            let response = call_inbound_service(inbound_service, Request::Peers).await?;
            let addrs = match response {
                Response::Peers(addrs) => addrs,
                _ => Vec::new(),
            };

            // The inbound service bounds its address responses; if it ever
            // returns more than the wire limit, the encoder reports a local
            // error, and the stream is reset without blaming the peer.
            let mut bytes = Vec::new();
            AddrResponse(addrs).encode(&mut bytes)?;
            write_response_bytes(send, &bytes).await
        }

        WireRequest::GetMempool => {
            unreachable!("get-mempool subscriptions are served by serve_mempool_subscription")
        }

        WireRequest::GetBlockRange {
            final_hash,
            count,
            max_bytes,
        } => {
            // Bound the bulk streams one peer holds open concurrently: each
            // commits this node to up to 64 MiB of block reads and
            // transfer. The peer sizes its work units to the bound, and a
            // refused stream is retried on another peer.
            let _slot = shared
                .active_bulk_streams
                .try_claim(MAX_CONCURRENT_BULK_STREAMS)
                .ok_or(ServeError::Refused)?;

            let mut delivered_count: u64 = 0;
            let mut delivered_bytes: u64 = 0;
            let mut next_hash = final_hash;
            let mut bytes = Vec::new();

            while delivered_count < count {
                let response = call_inbound_service(
                    inbound_service.clone(),
                    Request::BlocksByHash(std::iter::once(next_hash).collect()),
                )
                .await?;

                let block = match response {
                    Response::Blocks(mut blocks) => match blocks.pop() {
                        Some(InventoryResponse::Available((block, _))) => Some(block),
                        _ => None,
                    },
                    _ => None,
                };

                let Some(block) = block else {
                    if delivered_count == 0 {
                        // The anchor block is not held: a not-found result,
                        // with nothing following.
                        return write_response_bytes(send, &[RESULT_NOT_FOUND]).await;
                    }
                    // The chain ended (or the block is missing): finishing
                    // early is allowed, and the requester can resume from
                    // the last delivered block's parent hash.
                    return Ok(());
                };

                let block_bytes = block.zcash_serialize_to_vec().map_err(|err| {
                    ServeError::Encode(WireError::Local(format!("unserializable block: {err}")))
                })?;

                // Both request bounds are exact. The first block is always
                // delivered regardless of `max_bytes`, so a maximum-size
                // block stays retrievable; every later block must fit.
                if delivered_count > 0
                    && delivered_bytes.saturating_add(block_bytes.len() as u64) > max_bytes
                {
                    return Ok(());
                }

                bytes.clear();
                if delivered_count == 0 {
                    bytes.push(RESULT_OBJECT);
                }
                record::write_record(&mut bytes, &block_bytes).map_err(ServeError::Encode)?;
                write_response_bytes(send, &bytes).await?;

                delivered_count += 1;
                delivered_bytes = delivered_bytes.saturating_add(block_bytes.len() as u64);
                next_hash = block.header.previous_block_hash;
            }

            Ok(())
        }

        WireRequest::GetHashes {
            start_height,
            stride,
            count,
        } => {
            let response = call_inbound_service(
                inbound_service,
                Request::SyncHashes {
                    start_height,
                    stride,
                    // The wire request bounds the count at
                    // `MAX_GET_HASHES_COUNT` (50,000), so the cast is safe.
                    count: count as u32,
                },
            )
            .await?;
            let Response::SyncHashes(entries) = response else {
                return Err(ServeError::Refused);
            };

            let mut bytes = Vec::new();
            HashesResponse(entries).encode(&mut bytes)?;
            write_response_bytes(send, &bytes).await
        }

        WireRequest::GetTreeRoots {
            start_height,
            final_hash,
            count,
        } => {
            let response = call_inbound_service(
                inbound_service,
                Request::TreeRoots {
                    start_height,
                    final_hash,
                    // The wire request bounds the count at
                    // `MAX_GET_TREE_ROOTS_COUNT` (4,000), so the cast is
                    // safe.
                    count: count as u32,
                },
            )
            .await?;
            // Refuse when the anchor is not in the best chain, or the
            // index is unavailable, rather than serve entries for
            // different blocks.
            let Response::TreeRoots(Some(entries)) = response else {
                return Err(ServeError::Refused);
            };

            let mut bytes = Vec::new();
            TreeRootsResponse(entries).encode(&mut bytes)?;
            write_response_bytes(send, &bytes).await
        }

        WireRequest::GetObject {
            hash,
            offset,
            length,
        } => {
            // A node without an artifact store refuses `get-object`; it
            // also does not advertise `NODE_SYNC_ARTIFACTS`.
            let Some(artifact_dir) = shared.artifact_dir.clone() else {
                return Err(ServeError::Refused);
            };

            // Artifacts are named by the lowercase hex hash of their
            // contents, so a lookup cannot escape the artifact directory.
            let path = artifact_dir.join(hex::encode(hash.0));
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                // The object is not held: a not-found result, with nothing
                // following.
                Err(_) => return write_response_bytes(send, &[RESULT_NOT_FOUND]).await,
            };
            let size = file
                .metadata()
                .await
                .map_err(|err| {
                    ServeError::Encode(WireError::Local(format!("unreadable artifact: {err}")))
                })?
                .len();

            let mut bytes = vec![RESULT_OBJECT];
            record::write_compact_size(&mut bytes, size).map_err(ServeError::Encode)?;
            write_response_bytes(send, &bytes).await?;

            // Deliver up to `length` bytes from `offset`, and never a byte
            // past the object's size: both response bounds are exact. An
            // offset at or past the size delivers no data.
            if offset < size {
                use tokio::io::{AsyncReadExt, AsyncSeekExt};

                file.seek(std::io::SeekFrom::Start(offset))
                    .await
                    .map_err(|err| {
                        ServeError::Encode(WireError::Local(format!("unseekable artifact: {err}")))
                    })?;

                let mut remaining = length.min(size - offset);
                let mut buf = vec![0u8; 64 * 1024];
                while remaining > 0 {
                    // The cast is safe: `take` is at most the buffer size.
                    let take = (buf.len() as u64).min(remaining) as usize;
                    let read = file.read(&mut buf[..take]).await.map_err(|err| {
                        ServeError::Encode(WireError::Local(format!("unreadable artifact: {err}")))
                    })?;
                    if read == 0 {
                        // The file shrank while serving: finishing early is
                        // allowed, and the requester re-requests the rest.
                        break;
                    }
                    write_response_bytes(send, &buf[..read]).await?;
                    remaining -= read as u64;
                }
            }

            Ok(())
        }
    }
}

/// Calls the inbound service, mapping service unavailability to a stream
/// refusal.
async fn call_inbound_service<S>(
    inbound_service: S,
    request: Request,
) -> Result<Response, ServeError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    inbound_service.oneshot(request).await.map_err(|error| {
        // An overloaded inbound service refuses the request; the requester
        // can retry it on another peer.
        debug!(%error, "inbound service failed to serve v2 request");
        ServeError::Refused
    })
}

/// Writes response bytes to the stream, mapping errors to [`ServeError`].
async fn write_response_bytes(
    send: &mut quinn::SendStream,
    bytes: &[u8],
) -> Result<(), ServeError> {
    send.write_all(bytes).await.map_err(|_| ServeError::Write)
}

/// Reads the stream type byte of a stream the remote peer opened, failing
/// the connection if the peer finished the stream without one.
///
/// The peer must send the type byte promptly, so that a stream it opens and
/// then leaves idle does not pin its reader task.
///
/// Returns `None` if the type byte did not arrive: on a stream reset,
/// transport failure, or stall, the caller abandons the stream without
/// penalizing the peer.
async fn read_stream_type_or_fail(
    shared: &SharedConnection,
    recv: &mut quinn::RecvStream,
) -> Option<u8> {
    let type_byte = tokio::time::timeout(INBOUND_STREAM_TIMEOUT, record::read_u8(recv))
        .await
        .unwrap_or_else(|_elapsed| {
            Err(WireError::Timeout(
                "a stream opened without a stream type".to_string(),
            ))
        });

    match type_byte {
        Ok(type_byte) => Some(type_byte),
        Err(error) => {
            if !is_stream_failure(&error) {
                shared.fail_protocol("peer finished a stream without a stream type");
            }
            None
        }
    }
}

/// Handles a wire error reading an inbound request: peer violations fail the
/// connection, transport failures are ignored (the connection task reports
/// them).
fn handle_inbound_wire_error(shared: &SharedConnection, error: WireError) {
    if is_stream_failure(&error) {
        return;
    }

    // This node's own encoding failures never close the connection.
    if let WireError::Local(reason) = &error {
        warn!(%reason, "internal error handling an inbound v2 request");
        return;
    }

    // Scored violations leave the connection open: the peer is banned once
    // its accumulated score reaches the threshold.
    if let WireError::Misbehavior { points, reason } = &error {
        shared.report_misbehavior(*points, reason);
        return;
    }

    shared.fail_wire_error(&error);
}

/// Reads one inbound announcement stream for the life of the stream.
async fn read_inbound_announcement_stream<S>(
    shared: Arc<SharedConnection>,
    inbound_service: S,
    mut recv: quinn::RecvStream,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let Some(type_byte) = read_stream_type_or_fail(&shared, &mut recv).await else {
        return;
    };

    let stream_type = match StreamType::from_byte(type_byte) {
        Some(stream_type) if stream_type.is_announcement() => stream_type,
        Some(_bidirectional_type) => {
            shared.fail_protocol("bidirectional stream type on a unidirectional stream");
            return;
        }
        None => {
            let _ = recv.stop(ErrorCode::UnsupportedStreamType.into());
            return;
        }
    };

    // A node must not open a transaction announcement stream to a peer that
    // declined transaction relay.
    if stream_type == StreamType::TransactionAnnouncements && !shared.local_relay {
        shared.fail_protocol(
            "peer opened a transaction announcement stream after relay was declined",
        );
        return;
    }

    // Enforce at most one open announcement stream per type and direction.
    let newly_opened = shared
        .open_inbound_announcements
        .with(|open| open.insert(type_byte));
    if !newly_opened {
        shared.fail_protocol(format!(
            "peer opened a second concurrent announcement stream of type {type_byte:#04x}",
        ));
        return;
    }

    let mut recv = tokio::io::BufReader::with_capacity(RECORD_STREAM_READ_BUFFER_SIZE, recv);

    loop {
        // Announcement streams idle between records, so only a record the
        // peer has started and not finished is a stall.
        let payload = match record::read_record_timeout(&mut recv, INBOUND_STREAM_TIMEOUT).await {
            Ok(Some(payload)) => payload,
            Ok(None) => break,
            Err(error) => {
                handle_inbound_wire_error(&shared, error);
                break;
            }
        };

        if let Err(error) =
            handle_announcement(&shared, inbound_service.clone(), stream_type, &payload).await
        {
            handle_inbound_wire_error(&shared, error);
            break;
        }
    }

    // The stream ended: the peer may open a replacement.
    shared
        .open_inbound_announcements
        .with(|open| open.remove(&type_byte));
}

/// Handles one inbound announcement record.
async fn handle_announcement<S>(
    shared: &Arc<SharedConnection>,
    inbound_service: S,
    stream_type: StreamType,
    payload: &[u8],
) -> Result<(), WireError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    match stream_type {
        StreamType::TransactionAnnouncements => {
            let txref = TransactionReference::parse_exact(payload).await?;
            if txref.is_short_id() {
                return Err(WireError::Protocol(
                    "SHORTID reference in a transaction announcement".to_string(),
                ));
            }
            let id: UnminedTxId = txref
                .try_into()
                .expect("non-SHORTID references convert to unmined ids");

            register_advertised(shared, InventoryHash::from(id));

            let request = Request::AdvertiseTransactionIds([id].into(), shared.transient_addr);
            let _ = call_inbound_service(inbound_service, request).await;
        }

        StreamType::BlockAnnouncements => {
            let (&kind, body) = payload.split_first().ok_or_else(|| {
                WireError::Protocol("empty block announcement record".to_string())
            })?;

            let header_hash = match kind {
                // A header announcement.
                BLOCK_ANNOUNCEMENT_KIND_HEADER => {
                    let header = block::Header::zcash_deserialize(body)?;
                    header.hash()
                }
                // A compact block announcement: only conforming on
                // connections where this node requested high-bandwidth
                // announcements in its `init` record.
                BLOCK_ANNOUNCEMENT_KIND_COMPACT => {
                    if shared.hb_slot.is_none() {
                        return Err(WireError::Protocol(
                            "unrequested compact block announcement".to_string(),
                        ));
                    }

                    let mut body = body;
                    let compact = CompactBlock::read(&mut body).await?;
                    if !body.is_empty() {
                        return Err(WireError::Protocol(
                            "trailing bytes in a compact block announcement".to_string(),
                        ));
                    }

                    // The high-bandwidth penalty exemption only covers
                    // blocks whose header is valid: an announcement whose
                    // header fails its own proof of work is provable
                    // misbehavior, and its block is not worth
                    // reconstructing.
                    if !header_pow_is_valid(&compact.header) {
                        shared.report_misbehavior(
                            MISBEHAVIOR_PENALTY_INVALID_POW,
                            "compact block announcement with an invalid proof of work",
                        );
                        return Ok(());
                    }

                    register_advertised(shared, InventoryHash::Block(compact.header.hash()));
                    tokio::spawn(
                        reconstruct_compact_block(shared.clone(), inbound_service, compact)
                            .in_current_span(),
                    );
                    return Ok(());
                }
                unknown => {
                    return Err(WireError::Protocol(format!(
                        "unrecognized block announcement kind {unknown:#04x}",
                    )));
                }
            };

            register_advertised(shared, InventoryHash::Block(header_hash));

            // On connections where this node requested high-bandwidth
            // announcements, a header announcement may be the oversize
            // substitute for a compact block, sent before the announcer
            // fully validated the block: the advertiser is withheld from
            // the gossip pipeline, so a block that fails validation is not
            // penalized. The download is still routed to announcers by the
            // inventory registration above.
            let advertiser = if shared.hb_slot.is_some() {
                None
            } else {
                shared.transient_addr
            };
            let request = Request::AdvertiseBlock(header_hash, advertiser);
            let _ = call_inbound_service(inbound_service, request).await;
        }

        StreamType::AddressAnnouncements => {
            let addr = read_announced_addr(payload)?;
            if let Some(addr) = addr {
                // # Security
                //
                // Relayed addresses are rate-limited per connection with
                // the reference token bucket, so an address flood cannot
                // churn the address cache or the book. Dropped addresses
                // incur no penalty.
                let admitted = shared
                    .addr_tokens
                    .with(|tokens| tokens.try_take(std::time::Instant::now()));
                if admitted {
                    // Adds the address to the cache and bounds the cache
                    // size, like the legacy connection's unsolicited `addr`
                    // handling.
                    let no_response = shared
                        .cached_addrs
                        .with(|cached| update_addr_cache(cached, &[addr], None));
                    debug_assert!(no_response.is_empty(), "no response was requested");
                }
            }
        }

        _ => unreachable!("only announcement stream types reach this function"),
    }

    Ok(())
}

/// Parses a single announced `addrv2` address record.
fn read_announced_addr(payload: &[u8]) -> Result<Option<MetaAddr>, WireError> {
    use crate::protocol::external::addr::v2::AddrV2;

    let addr = AddrV2::zcash_deserialize(payload)?;

    Ok(MetaAddr::try_from(addr).ok())
}

/// Registers advertised inventory from this peer with the inventory
/// registry, so block and transaction downloads are routed to it.
fn register_advertised(shared: &SharedConnection, hash: InventoryHash) {
    if let Some(addr) = shared.transient_addr {
        let _ = shared
            .inv_collector
            .send(InventoryChange::new_available(hash, addr));
    }
}
