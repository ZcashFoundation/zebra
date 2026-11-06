//! The version 2 connection task.
//!
//! After the handshake, one task owns a connection for its whole life: the
//! QUIC connection, the open handshake stream, the peer's address announcement
//! budget, and the announcement writers. It accepts the peer's
//! streams, serves the peer's requests through the inbound service, drives
//! this node's requests to the peer, and presents the connection to the
//! peer set as an ordinary [`peer::Client`](crate::peer::Client).
//!
//! Each stream runs in its own task that holds only clones (the QUIC
//! connection handle, the inbound service, channel senders) and reports
//! connection-level outcomes to this task as an [`Event`]; a violation closes
//! the connection with the draft's code.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{
    channel::{mpsc as futures_mpsc, oneshot},
    FutureExt, StreamExt,
};
use quinn::{Connection, RecvStream, SendStream, VarInt};
use rand::RngExt;
use tokio::{
    sync::{mpsc, watch, Semaphore},
    task::JoinSet,
};
use tokio_util::codec::FramedRead;
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block, diagnostic::task::CheckForPanics, parameters::Network, serialization::DateTime32,
    transaction::UnminedTxId,
};

use crate::{
    meta_addr::MetaAddr,
    peer::{
        connection::overload_drop_connection_probability, CancelHeartbeatTask, Client,
        ClientRequest, ConnectionInfo, ErrorSlot, MustUseClientResponseSender, PeerError,
        SharedPeerError,
    },
    peer_set::{ConnectionTracker, InventoryChange},
    protocol::{
        external::types::PeerServices,
        internal::{Request, Response},
        v2::{
            announcement::{clamp_relayed_time, encode_addr_record, encode_tx_record},
            handshake::decode_record,
            object::ObjectSource,
            record::RecordCodec,
            types::{ErrorCode, StreamType},
            wire::WireError,
        },
    },
    quic::app_error_code,
    v2_address_book::{V2BookHandle, Via},
    BanList, BoxError, PeerSocketAddr,
};

use super::{
    addr_relay::{AddrRelayHandle, Registration, RELAY_CHANNEL_SIZE},
    handshake::CompletedHandshake,
    misbehavior::MisbehaviorScores,
};

mod addr_budget;
mod announcements;
mod inbound;
mod outbound;

pub(crate) use outbound::{fetch_object, Failure, ObjectSizes};

pub(crate) use outbound::{RequestSend, ResponseStream};

#[cfg(test)]
mod tests;

use addr_budget::AddrTokenBucket;
use announcements::{AnnouncementPermits, AnnouncementWriter};
pub(crate) use inbound::INBOUND_REQUEST_CONCURRENCY;

/// What this node provides to every version 2 connection.
#[derive(Clone)]
pub struct ConnectionParams<S> {
    /// The service that answers the peer's requests.
    pub inbound_service: S,

    /// Where advertised and missing inventory is registered for the peer set.
    pub inv_collector: tokio::sync::broadcast::Sender<InventoryChange>,

    /// The hash of the network's genesis block, which a `get-headers` response
    /// to a locator the peer does not know starts after (draft, "`get-headers`").
    pub genesis_hash: block::Hash,

    /// The network this connection belongs to, used to screen gossiped addresses.
    pub network: Network,

    /// The misbehavior scores of every version 2 peer, which report peers to
    /// ban to the address book.
    pub misbehavior: MisbehaviorScores,

    /// The version 2 address segment, which takes the addresses peers relay and answers their
    /// `get-addr` requests, or `None` for a connection that neither records nor serves
    /// addresses.
    pub(crate) address_book: Option<V2BookHandle>,

    /// Whether this node asks the peers it dialled for addresses, once each (draft,
    /// "`get-addr`").
    pub discovery: bool,

    /// The banned peers, whose addresses are neither kept nor relayed.
    pub bans: watch::Receiver<BanList>,

    /// The hub that relays addresses between version 2 peers and advertises
    /// this node's own; `None` for a connection that relays nothing.
    pub addr_relay: Option<AddrRelayHandle>,
    /// Whether the peer's `get-block-range` requests are served
    /// (`network.v2_serve_block_ranges`).
    pub serve_block_ranges: bool,

    /// The node-wide `get-block-range` stream permits, shared by every
    /// version 2 connection: [`global_block_range_permits`].
    pub block_range_permits: Arc<Semaphore>,

    /// The node-wide `get-hashes` permits, shared by every version 2
    /// connection: [`global_hashes_permits`].
    pub hashes_permits: Arc<Semaphore>,

    /// The synchronization artifacts this node serves through `get-object`
    /// requests, or `None` to refuse them all.
    pub object_source: Option<ObjectSource>,

    /// The services this node advertises in its `init` records, which decide
    /// which of the peer's requests it serves: `get-object` requests are
    /// refused unless they include `NODE_SYNC_ARTIFACTS`, whatever
    /// `object_source` holds.
    pub local_services: PeerServices,

    /// The permits for the `get-object` requests this node serves at once,
    /// over all its connections: [`global_object_permits`], shared by every
    /// connection's params.
    pub object_permits: Arc<Semaphore>,
}

/// Returns the permits for the `get-object` requests a node serves at once,
/// [`inbound::MAX_OBJECT_STREAMS_GLOBAL`] of them, for the
/// [`ConnectionParams`] of every connection the node makes.
pub fn global_object_permits() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(inbound::MAX_OBJECT_STREAMS_GLOBAL))
}

pub(crate) use inbound::{global_block_range_permits, global_hashes_permits};

/// Something a stream task needs the connection task to act on.
enum Event {
    /// Close the connection with `code`: the peer violated the protocol, or
    /// (with `INTERNAL_ERROR`) this node could not encode what it tried to
    /// send.
    Violation {
        /// What went wrong, for the error slot and the log.
        reason: String,
        /// The code to close with.
        code: ErrorCode,
    },

    /// Addresses the peer announced, for the version 2 address segment.
    Addresses(Vec<MetaAddr>),

    /// Close the connection because the peer's requests overloaded the
    /// inbound service, or timed out in it, and [`Overloads`] chose to drop
    /// this connection; `error` is [`PeerError::Overloaded`] or
    /// [`PeerError::InboundTimeout`].
    Overloaded(PeerError),

    /// Close the connection because the inbound service failed to become
    /// ready, which only happens when it has shut down, as the legacy
    /// connection does.
    ServiceShutdown,
}

/// How many best-effort events may wait for the connection task before new
/// address events are dropped.
const EVENT_QUEUE: usize = 64;

/// The most handshake-stream records of unknown kinds the connection task
/// reads in one [`UNKNOWN_HANDSHAKE_RECORD_INTERVAL`]: after that many, it
/// stops reading the handshake stream until the interval ends.
///
/// # Security
///
/// The draft has these records ignored, so that later revisions can define
/// new kinds (draft, "Connection Handshake": a node MUST ignore them), but
/// each one still costs a read of up to a maximum-size record. Pausing the
/// reads applies backpressure instead of closing the connection (draft,
/// "Misbehavior and Banning": rate limits and backpressure): once the
/// stream's receive window of unread records fills, the peer's flow control
/// stops it, and a conforming peer sending a few control records a minute is
/// never affected. While the reads are paused, a peer finishing its
/// handshake stream to disconnect is noticed when they resume.
const MAX_UNKNOWN_HANDSHAKE_RECORDS: u32 = 16;

/// The interval [`MAX_UNKNOWN_HANDSHAKE_RECORDS`] applies to.
const UNKNOWN_HANDSHAKE_RECORD_INTERVAL: Duration = Duration::from_secs(60);

/// Where the stream tasks this module spawns, for the peer's streams and for
/// this node's requests, send their events.
///
/// # Security
///
/// Events that close the connection have their own queue, which the
/// connection task reads before anything else, so the peer cannot delay or
/// crowd them out with best-effort events, such as the addresses it
/// announces, or with its own traffic. That holds for the violations this
/// node detects in the peer's responses, including the close that bans the
/// peer, as much as for those it detects in the peer's streams.
#[derive(Clone)]
struct Events {
    /// Connection-closing events. One is enough: the connection task closes
    /// the connection on the first.
    fatal: mpsc::Sender<Event>,
    /// Best-effort events: the addresses the peer sent.
    others: mpsc::Sender<Event>,
    /// Set by the first close handed to `fatal`, before the connection task
    /// reads it, so a [`ConnectionCloser`] sees the close at once.
    closing: Arc<AtomicBool>,
}

impl Events {
    /// The senders, and the receivers of the connection-closing and the
    /// other events.
    fn channel() -> (Self, mpsc::Receiver<Event>, mpsc::Receiver<Event>) {
        let (fatal, fatal_rx) = mpsc::channel(1);
        let (others, others_rx) = mpsc::channel(EVENT_QUEUE);
        let closing = Arc::new(AtomicBool::new(false));
        (
            Events {
                fatal,
                others,
                closing,
            },
            fatal_rx,
            others_rx,
        )
    }

    /// Hands a connection-closing event to the connection task, without
    /// waiting: if one is already queued, the connection is closing anyway,
    /// and if the task is gone, so is the connection.
    fn close(&self, event: Event) {
        self.closing.store(true, Ordering::Relaxed);
        let _ = self.fatal.try_send(event);
    }

    /// Hands addresses the peer announced to the connection task, best
    /// effort: they are dropped if the connection task is behind.
    fn addresses(&self, addrs: Vec<MetaAddr>) {
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.others.try_send(Event::Addresses(addrs))
        {
            metrics::counter!("zebra.net.in.addresses.dropped").increment(1);
        }
    }
}

/// A handle that closes one connection through its connection task, for code
/// outside this module that holds the connection: a
/// [`SyncPeer`](crate::SyncPeer) that found a violation in a bulk response.
///
/// It sends on the connection-closing queue, as the stream tasks do, so the
/// close goes through the connection task's error slot and log.
#[derive(Clone)]
pub(crate) struct ConnectionCloser {
    events: Events,
}

impl ConnectionCloser {
    /// Queues a close of the connection with `code` because of `reason`,
    /// without waiting, and marks the connection as closing at once (see
    /// [`ConnectionCloser::is_closed`]).
    ///
    /// The connection task closes the connection when it reads the close. If
    /// another close is already queued, the connection closes with that one's
    /// code instead; if the connection task is gone, the connection is closed
    /// or about to be anyway.
    pub(crate) fn close(&self, reason: String, code: ErrorCode) {
        self.events.close(Event::Violation { reason, code });
    }

    /// Whether a close has been queued, by this handle or by one of the
    /// connection's stream tasks, or the connection task has ended, however
    /// it ended.
    pub(crate) fn is_closed(&self) -> bool {
        self.events.closing.load(Ordering::Relaxed) || self.events.fatal.is_closed()
    }

    /// Waits until the connection task has ended, however it ended: even an
    /// aborted task, which does not close its connection explicitly.
    pub(crate) async fn closed(&self) {
        self.events.fatal.closed().await
    }
}

/// The connection's inbound service overloads, shared by its stream tasks:
/// the version 2 form of the legacy connection's overload policy, which
/// drops a connection at random on an overload or timeout, more likely the
/// more recently the connection last overloaded the inbound service (see
/// [`overload_drop_connection_probability`]).
///
/// # Security
///
/// Without it, a peer whose requests are shed or time out keeps its
/// connection and its share of the inbound service, and can retry for as
/// long as it likes.
struct Overloads {
    /// The time the overload times are measured from.
    origin: Instant,
    /// The time of the last overload, in milliseconds after `origin`, plus
    /// one; zero before the first overload.
    last: AtomicU64,
}

impl Overloads {
    fn new() -> Self {
        Overloads {
            origin: Instant::now(),
            last: AtomicU64::new(0),
        }
    }

    /// Records an inbound service failure `error` of a request from the peer
    /// or an announcement it made. Returns the event that closes the
    /// connection if `error` is an overload or timeout and the overload
    /// policy drops the connection for it.
    fn record(&self, error: &BoxError) -> Option<Event> {
        let error = if error.is::<tower::load_shed::error::Overloaded>() {
            PeerError::Overloaded
        } else if error.is::<tower::timeout::error::Elapsed>() {
            PeerError::InboundTimeout
        } else {
            return None;
        };

        let now = Instant::now();
        let since_origin = u64::try_from(now.duration_since(self.origin).as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let prev = match self.last.swap(since_origin, Ordering::Relaxed) {
            0 => None,
            prev => Some(self.origin + Duration::from_millis(prev - 1)),
        };
        let drop_connection_probability = overload_drop_connection_probability(now, prev);

        let overloaded = matches!(error, PeerError::Overloaded);
        if rand::rng().random::<f32>() < drop_connection_probability {
            if overloaded {
                metrics::counter!("pool.closed.loadshed").increment(1);
            } else {
                metrics::counter!("pool.closed.inbound.timeout").increment(1);
            }
            tracing::info!(
                drop_connection_probability = format!("{drop_connection_probability:.3}"),
                "inbound service {error} error, closing version 2 connection",
            );
            Some(Event::Overloaded(error))
        } else {
            if overloaded {
                metrics::counter!("pool.ignored.loadshed").increment(1);
            } else {
                metrics::counter!("pool.ignored.inbound.timeout").increment(1);
            }
            None
        }
    }
}

/// The inbound service, with the connection's record of the overloads the
/// peer's requests and announcements cause.
#[derive(Clone)]
struct Inbound<'a, S> {
    service: S,
    overloads: &'a Overloads,
}

/// Why an inbound service call failed, as the connection acts on it.
enum ServiceFailure {
    /// Close the connection: the inbound service has shut down, or the
    /// connection's overload policy dropped the connection for the call's
    /// overload or timeout; `Event` says which.
    Close(Event),
    /// The call failed with this error, and the connection stays open.
    Failed(BoxError),
}

impl<S> Inbound<'_, S>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    /// Calls the inbound service, counting the call's overload or timeout
    /// towards closing the connection.
    ///
    /// # Security
    ///
    /// The inbound service is wrapped in buffer, load-shed, and timeout
    /// layers by the caller. Overloads and timeouts count towards closing
    /// the connection, as on the legacy connection, and the load-shed layer
    /// is always ready, so a readiness error means the inbound service has
    /// shut down: close the connection, as the legacy connection does.
    async fn call(&mut self, request: Request) -> Result<Response, ServiceFailure> {
        let command = request.command();
        if self.service.ready().await.is_err() {
            tracing::debug!(command, "inbound service shut down, closing the connection");
            return Err(ServiceFailure::Close(Event::ServiceShutdown));
        }
        self.service
            .call(request)
            .await
            .map_err(|error| match self.overloads.record(&error) {
                Some(event) => ServiceFailure::Close(event),
                None => ServiceFailure::Failed(error),
            })
    }
}

/// The error slot entry for a connection closed with `code`.
///
/// `INTERNAL_ERROR` is only ever this node's own failure (a value it could
/// not encode), so the peer is not blamed for it.
pub(super) fn close_error(reason: String, code: ErrorCode) -> PeerError {
    match code {
        ErrorCode::InternalError => PeerError::LocalEncodingError(reason),
        _ => PeerError::ProtocolViolation(reason),
    }
}

/// Spawns the connection task for a completed handshake and returns the
/// [`Client`] the peer set uses to talk to the peer, and a
/// [`ConnectionCloser`] for the connection's [`SyncPeer`](crate::SyncPeer).
///
/// `exit_notifier` is dropped when the connection task ends, however it
/// ends, so a caller holding the receiver learns the connection is gone.
///
/// `object_sizes` is the connection's own size task, shared with the
/// connection's [`SyncPeer`](crate::SyncPeer) so that every `get-object`
/// answer on the connection is checked against the same earlier sizes.
pub fn spawn_connection<S>(
    connection: Connection,
    completed: CompletedHandshake,
    connection_tracker: ConnectionTracker,
    params: ConnectionParams<S>,
    object_sizes: ObjectSizes,
    exit_notifier: Option<tokio::sync::oneshot::Sender<()>>,
) -> (Client, ConnectionCloser)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let CompletedHandshake {
        connection_info,
        send,
        recv,
    } = completed;
    let transient_addr = connection_info.connected_addr.get_transient_addr();
    let addr_label = connection_info.connected_addr.get_transient_addr_label();

    // One queued request per client, as on the legacy connection. How many are in flight is bounded
    // separately, by the connection task's outbound permits.
    let (server_tx, server_rx) = futures_mpsc::channel::<ClientRequest>(0);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<CancelHeartbeatTask>();
    let error_slot = ErrorSlot::default();
    let (events, fatal_rx, events_rx) = Events::channel();
    let closer = ConnectionCloser {
        events: events.clone(),
    };

    let connection_span = tracing::debug_span!("v2_connection", %addr_label);

    // Register with the relay hub here, before the task runs, so the hub's messages wait in the
    // channel for the task.
    let (relay_rx, relay_registration) = match (&params.addr_relay, transient_addr) {
        (Some(addr_relay), Some(peer)) => {
            let (relay_tx, relay_rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
            (Some(relay_rx), Some(addr_relay.register(peer, relay_tx)))
        }
        _ => (None, None),
    };

    let is_inbound = connection_info.connected_addr.is_inbound();
    let block_ranges = inbound::BlockRangeServing::new(
        params.serve_block_ranges,
        params.block_range_permits.clone(),
    );
    let inbound_permits = inbound::StreamPermits::new(params.hashes_permits.clone());
    let objects = inbound::ObjectServing {
        source: params.object_source.clone(),
        advertised: params
            .local_services
            .contains(PeerServices::NODE_SYNC_ARTIFACTS),
        peer_permits: Arc::new(Semaphore::new(inbound::MAX_OBJECT_STREAMS_PER_PEER)),
        global_permits: params.object_permits.clone(),
        stall_backoff: Arc::new(inbound::hashes::StallBackoff::new()),
    };
    let task = ConnectionTask {
        connection: connection.clone(),
        is_inbound,
        get_addr_allowed: Arc::new(AtomicBool::new(is_inbound)),
        connection_info: connection_info.clone(),
        transient_addr,
        handshake_recv: recv,
        _handshake_send: send,
        params,
        client_rx: server_rx.into(),
        error_slot: error_slot.clone(),
        _connection_tracker: connection_tracker,
        addr_budget: AddrTokenBucket::new(tokio::time::Instant::now()),
        relay_rx,
        relay_registration,
        tx_announcer: None,
        addr_announcer: None,
        announcement_permits: AnnouncementPermits::new(),
        inbound_permits,
        block_ranges,
        overloads: Arc::new(Overloads::new()),
        objects,
        outbound_permits: Arc::new(Semaphore::new(outbound::MAX_OUTBOUND_REQUEST_STREAMS)),
        object_sizes,
        tasks: JoinSet::new(),
        events,
        fatal_rx,
        events_rx,
        unknown_handshake_records: 0,
        unknown_handshake_records_since: tokio::time::Instant::now(),
        handshake_paused_until: None,
        _exit_notifier: exit_notifier,
    };
    let inv_collector = task.params.inv_collector.clone();

    let connection_task = tokio::spawn(task.run().instrument(connection_span).boxed());

    // QUIC keep-alive packets replace the legacy ping/pong. The client still polls a "heartbeat"
    // task and treats its end as a failure, so one exists that only ends on the client's shutdown
    // signal.
    let heartbeat_task = tokio::spawn(async move {
        let _ = shutdown_rx.await;
        Err::<(), BoxError>(PeerError::ClientCancelledHeartbeatTask.into())
    });

    // Version 2 connections take no block adverts or compact block relay yet. The peer set's
    // advertiser skips a closed advert channel, so v2 peers neither receive adverts nor count
    // towards the advert fanout; the compact receive slot is only offered to connections that
    // negotiated early block relay.
    let (block_advert_tx, _) = mpsc::channel(1);
    let (compact_receive_tx, _) = tokio::sync::watch::channel(None);

    let client = Client {
        connection_info,
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        block_advert_tx,
        compact_receive_tx,
        inv_collector,
        error_slot,
        connection_task,
        heartbeat_task,
    };
    (client, closer)
}

/// The task that owns a connection.
struct ConnectionTask<S> {
    connection: Connection,
    /// Whether the peer dialled this node.
    is_inbound: bool,
    /// Whether the peer may still send a `get-addr` request: only once, and
    /// only on a connection the peer dialled (draft, "`get-addr`"). Shared
    /// with the request stream tasks.
    get_addr_allowed: Arc<AtomicBool>,
    connection_info: Arc<ConnectionInfo>,
    transient_addr: Option<PeerSocketAddr>,
    handshake_recv: FramedRead<RecvStream, RecordCodec>,
    /// Kept open for the connection's life; finishing it would signal intent to disconnect.
    _handshake_send: SendStream,
    params: ConnectionParams<S>,
    client_rx: crate::peer::ClientRequestReceiver,
    error_slot: ErrorSlot,
    /// Reduces the open connection count when dropped.
    _connection_tracker: ConnectionTracker,
    /// The budget for the address records the peer announces, on all its address announcement
    /// streams.
    addr_budget: AddrTokenBucket,
    /// The addresses the relay hub has for the peer; `None` without a hub, or once it is gone.
    relay_rx: Option<mpsc::Receiver<Vec<MetaAddr>>>,
    /// This connection's registration with the relay hub.
    relay_registration: Option<Registration>,
    /// This node's transaction announcement stream to the peer, opened on first use. Block
    /// announcements need a header the internal request does not carry.
    tx_announcer: Option<AnnouncementWriter>,
    /// This node's address announcement stream to the peer, opened on first use.
    addr_announcer: Option<AnnouncementWriter>,
    announcement_permits: AnnouncementPermits,
    /// Bounds how many of the peer's requests are in the inbound service at once.
    inbound_permits: inbound::StreamPermits,
    /// Bounds how many of the peer's `get-block-range` requests are served at once.
    block_ranges: inbound::BlockRangeServing,
    /// The peer's inbound service overloads, which may close the connection.
    overloads: Arc<Overloads>,
    /// What serving the peer's `get-object` requests needs, with the peer's
    /// own object stream permits.
    objects: inbound::ObjectServing,
    /// Bounds how many of this node's requests are in flight to the peer at once.
    outbound_permits: Arc<Semaphore>,
    /// The object sizes the peer gave in its `get-object` answers to this node.
    object_sizes: ObjectSizes,
    tasks: JoinSet<()>,
    events: Events,
    /// Connection-closing events from the stream tasks and this node's
    /// request tasks.
    fatal_rx: mpsc::Receiver<Event>,
    /// Best-effort events from the stream tasks and this node's request
    /// tasks.
    events_rx: mpsc::Receiver<Event>,
    /// Handshake-stream records of unknown kinds since
    /// `unknown_handshake_records_since`.
    unknown_handshake_records: u32,
    /// When the current [`UNKNOWN_HANDSHAKE_RECORD_INTERVAL`] started.
    unknown_handshake_records_since: tokio::time::Instant,
    /// When the connection task resumes reading the handshake stream, after
    /// [`MAX_UNKNOWN_HANDSHAKE_RECORDS`] in the current interval; `None`
    /// while it reads it.
    handshake_paused_until: Option<tokio::time::Instant>,
    /// Dropped when the task ends, which tells the holder of the receiver.
    _exit_notifier: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Why the connection task is ending.
struct Exit {
    error: PeerError,
    code: ErrorCode,
}

impl<S> ConnectionTask<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    /// The event loop.
    async fn run(mut self) {
        if !self.is_inbound && self.params.discovery {
            self.spawn_get_addr();
        }
        let exit = self.run_loop().await;

        tracing::debug!(error = %exit.error, code = ?exit.code, "closing version 2 connection");
        // The reason is for the peer's logs: a close the peer caused carries it, any other close
        // sends only the code's name, so this node's internals stay in its own logs.
        let reason = match exit.code {
            ErrorCode::NoError | ErrorCode::InternalError => exit.code.name().to_string(),
            _ => exit.error.to_string(),
        };
        self.connection
            .close(app_error_code(exit.code), reason.as_bytes());
        let _ = self.error_slot.try_update_error(exit.error.into());

        // Fail any client request that is still queued.
        while let Some(request) = self.client_rx.close_and_flush_next() {
            let error: SharedPeerError = self
                .error_slot
                .try_get_error()
                .unwrap_or_else(|| PeerError::ConnectionDropped.into());
            let _ = request.tx.send(Err(error));
        }

        self.tx_announcer = None;
        self.addr_announcer = None;
        self.relay_rx = None;
        if let (Some(addr_relay), Some(registration)) =
            (&self.params.addr_relay, self.relay_registration.take())
        {
            addr_relay.deregister(registration);
        }

        // Dropping `tasks` aborts the stream tasks: after `close` nothing they do can reach the
        // peer, every `ResponseGuard` answers its client on drop, and the stream tasks are already
        // cancellation-safe because `Client::shutdown` aborts this task the same way.
    }

    /// Runs until something ends the connection.
    async fn run_loop(&mut self) -> Exit {
        loop {
            tokio::select! {
                // Prefer task events and the peer's traffic over new local requests, like the
                // legacy connection. Events come first, connection-closing ones before the rest,
                // so the peer's traffic cannot hold them back.
                biased;

                event = self.fatal_rx.recv() => {
                    // The task holds a sender, so the channel never closes while it runs.
                    let Some(event) = event else { continue };
                    if let Some(exit) = self.handle_event(event) {
                        return exit;
                    }
                }

                event = self.events_rx.recv() => {
                    // The task holds a sender, so the channel never closes while it runs.
                    let Some(event) = event else { continue };
                    if let Some(exit) = self.handle_event(event) {
                        return exit;
                    }
                }

                relayed = recv_relayed(&mut self.relay_rx), if self.relay_rx.is_some() => match relayed {
                    Some(addrs) => self.announce_addresses(addrs),
                    // The hub dropped this connection, or has exited.
                    None => self.relay_rx = None,
                },

                record = self.handshake_recv.next(), if self.handshake_paused_until.is_none() => {
                    if let Some(exit) = self.handle_handshake_record(record) {
                        return exit;
                    }
                }

                () = tokio::time::sleep_until(
                    self.handshake_paused_until.unwrap_or_else(tokio::time::Instant::now),
                ), if self.handshake_paused_until.is_some() => {
                    self.handshake_paused_until = None;
                }

                accepted = self.connection.accept_bi() => match accepted {
                    Ok((send, recv)) => self.spawn_inbound_stream(send, recv),
                    Err(error) => return Self::connection_ended(error),
                },

                accepted = self.connection.accept_uni() => match accepted {
                    Ok(recv) => {
                        self.tasks.spawn(announcements::serve(
                            recv,
                            self.announcement_permits.clone(),
                            self.params.network.clone(),
                            self.transient_addr,
                            self.params.inbound_service.clone(),
                            self.overloads.clone(),
                            self.params.inv_collector.clone(),
                            self.events.clone(),
                        ));
                    }
                    Err(error) => return Self::connection_ended(error),
                },

                finished = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Some(result) = finished {
                        let _ = result.panic_if_task_has_panicked();
                    }
                }

                // While every outbound permit is in use, the next request stays queued, so the
                // client is unready, as a legacy peer is while it answers; a finishing stream task
                // wakes this loop through `join_next` above.
                request = self.client_rx.next(), if self.outbound_permits.available_permits() > 0 => match request {
                    Some(request) => {
                        let span = request.span.clone();
                        self.handle_client_request(request).instrument(span).await;
                    }
                    None => return Exit {
                        error: PeerError::ClientDropped,
                        code: ErrorCode::NoError,
                    },
                },
            }
        }
    }

    /// Handles a record on the handshake stream after the handshake, per the
    /// contract on [`CompletedHandshake::recv`].
    fn handle_handshake_record(
        &mut self,
        record: Option<Result<Bytes, WireError>>,
    ) -> Option<Exit> {
        match record {
            // The peer finished the handshake stream (it intends to disconnect), or the transport
            // ended underneath it.
            None | Some(Err(WireError::Io(_))) => Some(Exit {
                error: PeerError::ConnectionClosed,
                code: ErrorCode::NoError,
            }),
            Some(Err(error)) => Some(Exit {
                error: PeerError::ProtocolViolation(format!("handshake stream: {error}")),
                code: error.close_code(),
            }),
            Some(Ok(payload)) => match decode_record(&payload) {
                // A second init record is a protocol error (draft, "Handshake Validation").
                Ok(Some(_)) => Some(Exit {
                    error: PeerError::DuplicateHandshake,
                    code: ErrorCode::ProtocolError,
                }),
                Ok(None) => {
                    // `decode_record` reads the kind byte, so the payload is not empty.
                    tracing::debug!(
                        kind = payload[0],
                        "ignoring handshake record of unknown kind"
                    );
                    let now = tokio::time::Instant::now();
                    if now.duration_since(self.unknown_handshake_records_since)
                        >= UNKNOWN_HANDSHAKE_RECORD_INTERVAL
                    {
                        self.unknown_handshake_records_since = now;
                        self.unknown_handshake_records = 0;
                    }
                    self.unknown_handshake_records += 1;
                    if self.unknown_handshake_records >= MAX_UNKNOWN_HANDSHAKE_RECORDS {
                        tracing::debug!(
                            ?UNKNOWN_HANDSHAKE_RECORD_INTERVAL,
                            "too many handshake records of unknown kinds, \
                             pausing reads of the handshake stream"
                        );
                        self.handshake_paused_until = Some(
                            self.unknown_handshake_records_since
                                + UNKNOWN_HANDSHAKE_RECORD_INTERVAL,
                        );
                    }
                    None
                }
                Err(error) => Some(Exit {
                    error: PeerError::ProtocolViolation(format!("handshake stream: {error}")),
                    code: ErrorCode::ProtocolError,
                }),
            },
        }
    }

    /// Handles an event from a stream task.
    fn handle_event(&mut self, event: Event) -> Option<Exit> {
        match event {
            Event::Violation { reason, code } => Some(Exit {
                error: close_error(reason, code),
                code,
            }),
            Event::Addresses(addrs) => {
                // # Security
                //
                // Announced addresses pass the peer's address budget, are offered for relay to
                // other version 2 peers, and go to the version 2 address segment, which admits
                // them through its intake rules and buckets; the peer cannot fill the address
                // books directly, and version 2 addresses never reach the legacy one.
                self.accept_announced_addresses(addrs);
                None
            }
            // Like the legacy connection, an overload drop blames the inbound service's load,
            // not a protocol violation: the draft leaves overload to disconnection without a ban
            // (draft, "Misbehavior and Banning"), so the close is graceful.
            Event::Overloaded(error) => Some(Exit {
                error,
                code: ErrorCode::NoError,
            }),
            // This node is shutting down, or its inbound service failed: the peer is not to blame.
            Event::ServiceShutdown => Some(Exit {
                error: PeerError::ServiceShutdown,
                code: ErrorCode::NoError,
            }),
        }
    }

    /// Spawns the task that serves one bidirectional stream the peer opened.
    fn spawn_inbound_stream(&mut self, send: SendStream, recv: RecvStream) {
        self.tasks.spawn(inbound::serve(
            send,
            recv,
            self.params.inbound_service.clone(),
            self.inbound_permits.clone(),
            self.overloads.clone(),
            self.get_addr_allowed.clone(),
            self.params.address_book.clone(),
            self.events.clone(),
            self.block_ranges.clone(),
            self.objects.clone(),
        ));
    }

    /// Sends the peer this node's one `get-addr` request, on a connection this node dialled
    /// (draft, "`get-addr`"), and hands the whole answer to the version 2 address segment.
    ///
    /// The request is driven like a client's, so an oversized or malformed answer is penalized
    /// or closes the connection in the same way.
    fn spawn_get_addr(&mut self) {
        let (Some(book), Some(source)) = (self.params.address_book.clone(), self.transient_addr)
        else {
            return;
        };
        let Ok(permit) = self.outbound_permits.clone().try_acquire_owned() else {
            // Only this task takes permits, and it takes none before this.
            return;
        };
        let (tx, rx) = oneshot::channel();
        let request = Request::Peers;
        let tx = MustUseClientResponseSender::new(tx, &request, None, Some(source));
        // The guard is created before the task is spawned, so if the task is aborted before it
        // first runs, dropping it still answers `rx`.
        let guard = outbound::ResponseGuard::new(tx);
        let context = self.request_context();
        self.tasks.spawn(async move {
            outbound::drive(request, guard, permit, context).await;
            if let Ok(Ok(Response::Peers(addrs))) = rx.await {
                book.intake(source, addrs, Via::GetAddr);
            }
        });
    }

    /// What a request this node sends to the peer needs from the connection.
    fn request_context(&self) -> outbound::RequestContext {
        outbound::RequestContext {
            connection: self.connection.clone(),
            transient_addr: self.transient_addr,
            genesis_hash: self.params.genesis_hash,
            misbehavior: self.params.misbehavior.clone(),
            events: self.events.clone(),
            object_sizes: self.object_sizes.clone(),
        }
    }

    /// Handles a request from Zebra's components.
    async fn handle_client_request(&mut self, request: crate::peer::InProgressClientRequest) {
        let crate::peer::InProgressClientRequest { request, tx, .. } = request;

        if tx.is_canceled() {
            metrics::counter!(
                "zebra.net.out.requests.canceled",
                "command" => request.command(),
            )
            .increment(1);
            return;
        }
        metrics::counter!(
            "zebra.net.out.requests",
            "command" => request.command(),
        )
        .increment(1);

        let response = match request {
            // The legacy address book's crawler does not ask version 2 peers for addresses: their
            // records are QUIC endpoints, which the connection hands to the version 2 address
            // segment instead. The peer set does not route this request here, so this only
            // answers a direct caller.
            Request::Peers => Ok(Response::Peers(Vec::new())),

            // The transport measures round-trip time; no message is needed.
            Request::Ping(_) => Ok(Response::Pong(self.connection.rtt())),

            // Announcements go on this node's announcement streams, best effort.
            Request::AdvertiseTransactionIds(ids, _) => {
                self.announce_transactions(ids);
                Ok(Response::Nil)
            }
            Request::PushTransaction(transaction, _) => {
                // There is no push on the version 2 protocol: announce the ID, and the peer
                // fetches the transaction with `get-tx`.
                self.announce_transactions([transaction.id]);
                Ok(Response::Nil)
            }
            // Block adverts use the peer set's dedicated advertiser, which skips version 2
            // connections: a block announcement carries the header, which the advert does not,
            // so v2 peers learn new blocks from other peers for now.
            Request::AdvertiseBlock(..)
            | Request::AdvertiseBlockToAll(_)
            | Request::AdvertiseEarlyBlock(_) => {
                Err(PeerError::AdvertRequiresDedicatedService.into())
            }

            // Requests the connection makes of the inbound service are never sent to a peer.
            Request::EarlyBlocksByHash(_)
            | Request::AdvertiseCompactBlock(..)
            | Request::CheckCompactHeader(_)
            | Request::CompactBlockTransactions(_)
            | Request::CheckCompactBlock(_)
            | Request::BlockRange { .. }
            | Request::SyncHashes { .. } => Err(PeerError::InboundOnlyRequest.into()),

            // Peer set internal.
            Request::PollPeerSet => Ok(Response::Nil),

            // Everything else is a request stream, driven in its own task.
            request => {
                let permit = self
                    .outbound_permits
                    .clone()
                    .try_acquire_owned()
                    .expect("requests are only taken while a permit is available, and only this task takes permits");
                let context = self.request_context();
                // The guard is created before the task is spawned, so if the task is aborted
                // before it first runs, dropping it still answers the client.
                self.tasks.spawn(outbound::drive(
                    request,
                    outbound::ResponseGuard::new(tx),
                    permit,
                    context,
                ));
                return;
            }
        };

        let _ = tx.send(response);
    }

    /// Applies the peer's address budget to a batch of addresses it
    /// announced, drops banned ones, offers the rest to the relay hub with
    /// their times clamped, and hands them to the version 2 address segment.
    ///
    /// # Security
    ///
    /// Records over the budget are dropped without penalty (draft,
    /// "Misbehavior and Banning": rate limits), so a peer can't flood the
    /// relay hub or the address segment, whichever of its streams it uses.
    ///
    /// Announced addresses are version 2 (QUIC) endpoints, so they are only
    /// relayed to other version 2 peers and recorded in the version 2 address
    /// segment: they never reach the legacy address book, whose entries are
    /// dialled over TCP.
    fn accept_announced_addresses(&mut self, addrs: Vec<MetaAddr>) {
        let now = tokio::time::Instant::now();
        let wall_now = DateTime32::now();
        let bans = self.params.bans.borrow().clone();

        let mut rate_limited: u64 = 0;
        let mut accepted = Vec::with_capacity(addrs.len());
        for addr in addrs {
            if !self.addr_budget.try_take(now) {
                rate_limited += 1;
                continue;
            }
            if bans.is_banned(addr.addr().ip()) {
                continue;
            }
            accepted.push(addr);
        }
        if rate_limited > 0 {
            metrics::counter!("zcash.net.v2.addresses.rate_limited").increment(rate_limited);
        }

        // How many records arrived together depends on network timing, not on the peer, so
        // every accepted record is offered; the budget bounds the rate.
        let Some(source) = self.transient_addr else {
            return;
        };
        if accepted.is_empty() {
            return;
        }
        if let Some(addr_relay) = &self.params.addr_relay {
            let clamped = accepted
                .iter()
                .map(|addr| with_clamped_time(addr.clone(), wall_now))
                .collect();
            addr_relay.offer(source, clamped);
        }
        // The segment clamps the times itself, and counts the records it clamped.
        if let Some(book) = &self.params.address_book {
            book.intake(source, accepted, Via::Announcement);
        }
    }

    /// Queues the addresses the relay hub has for the peer as one batch of
    /// address announcement records, opening the stream on first use or after
    /// the previous writer ended. Addresses banned since they were offered are
    /// skipped.
    fn announce_addresses(&mut self, addrs: Vec<MetaAddr>) {
        let bans = self.params.bans.borrow().clone();
        let mut batch = Vec::new();
        for addr in addrs {
            if bans.is_banned(addr.addr().ip()) {
                continue;
            }
            // Relayed addresses always have their services and time, and a record is far under
            // the record limit; a failure is logged and the address skipped.
            match encode_addr_record(&addr).and_then(RecordCodec::encode_record) {
                Ok(record) => batch.extend_from_slice(&record),
                Err(error) => tracing::debug!(%error, "skipping unencodable address"),
            }
        }
        if batch.is_empty() {
            return;
        }

        let writer = self.addr_announcer.get_or_insert_with(|| {
            AnnouncementWriter::spawn(
                self.connection.clone(),
                StreamType::AddressAnnouncements,
                &mut self.tasks,
                crate::constants::REQUEST_TIMEOUT,
            )
        });
        if writer.announce(batch.into()).is_err() {
            // The writer's stream ended; the next batch opens a replacement.
            self.addr_announcer = None;
        }
    }

    /// Queues one batch of transaction announcement records, opening the
    /// stream on first use or after the previous writer ended. Skipped for a
    /// peer that declined relay.
    fn announce_transactions(&mut self, ids: impl IntoIterator<Item = UnminedTxId>) {
        if !self.connection_info.remote.relay() {
            return;
        }
        // One framed record per ID. A reference is at most 65 bytes, far under the record limit,
        // so encoding cannot fail; a failure is logged and the ID skipped rather than trusted.
        let mut batch = Vec::new();
        for id in ids {
            match RecordCodec::encode_record(encode_tx_record(id)) {
                Ok(record) => batch.extend_from_slice(&record),
                Err(error) => tracing::debug!(%error, "skipping unencodable announcement"),
            }
        }
        if batch.is_empty() {
            return;
        }

        let writer = self.tx_announcer.get_or_insert_with(|| {
            // Bounded like a response write.
            AnnouncementWriter::spawn(
                self.connection.clone(),
                StreamType::TransactionAnnouncements,
                &mut self.tasks,
                crate::constants::REQUEST_TIMEOUT,
            )
        });
        if writer.announce(batch.into()).is_err() {
            // The writer's stream ended; the next batch opens a replacement.
            self.tx_announcer = None;
        }
    }

    /// The exit for a QUIC connection that ended underneath the task.
    fn connection_ended(error: quinn::ConnectionError) -> Exit {
        if let quinn::ConnectionError::ApplicationClosed(close) = &error {
            tracing::debug!(
                code = close.error_code.into_inner(),
                reason = %String::from_utf8_lossy(&close.reason),
                "peer closed the connection"
            );
        }
        let error = match error {
            quinn::ConnectionError::ApplicationClosed(_)
            | quinn::ConnectionError::ConnectionClosed(_)
            | quinn::ConnectionError::TimedOut
            | quinn::ConnectionError::Reset => PeerError::ConnectionClosed,
            quinn::ConnectionError::LocallyClosed => PeerError::ConnectionDropped,
            other => PeerError::ProtocolViolation(format!("transport error: {other}")),
        };
        Exit {
            error,
            code: ErrorCode::NoError,
        }
    }
}

/// Receives the relay hub's next batch for the peer, or waits forever
/// without a hub.
async fn recv_relayed(
    relay_rx: &mut Option<mpsc::Receiver<Vec<MetaAddr>>>,
) -> Option<Vec<MetaAddr>> {
    match relay_rx {
        Some(relay_rx) => relay_rx.recv().await,
        None => std::future::pending().await,
    }
}

/// `addr` with its relayed time clamped (draft, "addrv2 Records"). An address
/// without a time is kept as it is; decoded records always have one.
fn with_clamped_time(addr: MetaAddr, now: DateTime32) -> MetaAddr {
    match (addr.services(), addr.last_seen()) {
        (Some(services), Some(time)) => {
            MetaAddr::new_gossiped_meta_addr(addr.addr(), services, clamp_relayed_time(time, now))
        }
        _ => addr,
    }
}

/// Refuses a stream the peer opened: cancels the peer's sending direction
/// and, for a bidirectional stream, resets this node's.
pub(super) fn refuse_stream(send: Option<&mut SendStream>, recv: &mut RecvStream, code: ErrorCode) {
    let code: VarInt = app_error_code(code);
    // A stream that is already closed needs no refusing.
    let _ = recv.stop(code);
    if let Some(send) = send {
        let _ = send.reset(code);
    }
}
