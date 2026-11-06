//! The version 2 connection task.
//!
//! After the handshake, one task owns a connection for its whole life: the
//! QUIC connection, the open handshake stream, the cache of addresses the
//! peer announced, and the announcement writers. It accepts the peer's
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
    sync::{mpsc, Semaphore},
    task::JoinSet,
};
use tokio_util::codec::FramedRead;
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{block, diagnostic::task::CheckForPanics, transaction::UnminedTxId};

use crate::{
    meta_addr::MetaAddr,
    peer::{
        connection::{overload_drop_connection_probability, Handler},
        CancelHeartbeatTask, Client, ClientRequest, ConnectionInfo, ErrorSlot, PeerError,
        SharedPeerError,
    },
    peer_set::{ConnectionTracker, InventoryChange},
    protocol::{
        internal::{Request, Response},
        v2::{
            announcement::encode_tx_record,
            handshake::decode_record,
            record::RecordCodec,
            types::{ErrorCode, StreamType},
            wire::WireError,
        },
    },
    quic::app_error_code,
    BoxError, PeerSocketAddr,
};

use super::{handshake::CompletedHandshake, misbehavior::MisbehaviorScores};

mod announcements;
mod inbound;
mod outbound;

#[cfg(test)]
mod tests;

use announcements::{AnnouncementPermits, AnnouncementWriter};

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

    /// The misbehavior scores of every version 2 peer, which report peers to
    /// ban to the address book.
    pub misbehavior: MisbehaviorScores,
}

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

    /// Addresses from the peer: announced, or left over from a `get-addr`
    /// answer after the client's sample; cached for `Request::Peers`.
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
}

impl Events {
    /// The senders, and the receivers of the connection-closing and the
    /// other events.
    fn channel() -> (Self, mpsc::Receiver<Event>, mpsc::Receiver<Event>) {
        let (fatal, fatal_rx) = mpsc::channel(1);
        let (others, others_rx) = mpsc::channel(EVENT_QUEUE);
        (Events { fatal, others }, fatal_rx, others_rx)
    }

    /// Hands a connection-closing event to the connection task, without
    /// waiting: if one is already queued, the connection is closing anyway,
    /// and if the task is gone, so is the connection.
    fn close(&self, event: Event) {
        let _ = self.fatal.try_send(event);
    }

    /// Hands addresses the peer sent to the connection task's cache, best
    /// effort: they are dropped if the connection task is behind.
    fn addresses(&self, addrs: Vec<MetaAddr>) {
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.others.try_send(Event::Addresses(addrs))
        {
            metrics::counter!("zebra.net.in.addresses.dropped").increment(1);
        }
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
/// [`Client`] the peer set uses to talk to the peer.
///
/// `exit_notifier` is dropped when the connection task ends, however it
/// ends, so a caller holding the receiver learns the connection is gone.
pub fn spawn_connection<S>(
    connection: Connection,
    completed: CompletedHandshake,
    connection_tracker: ConnectionTracker,
    params: ConnectionParams<S>,
    exit_notifier: Option<tokio::sync::oneshot::Sender<()>>,
) -> Client
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

    let connection_span = tracing::debug_span!("v2_connection", %addr_label);

    let is_inbound = connection_info.connected_addr.is_inbound();
    let task = ConnectionTask {
        connection: connection.clone(),
        is_inbound,
        sent_get_addr: false,
        get_addr_allowed: Arc::new(AtomicBool::new(is_inbound)),
        connection_info: connection_info.clone(),
        transient_addr,
        handshake_recv: recv,
        _handshake_send: send,
        params,
        client_rx: server_rx.into(),
        error_slot: error_slot.clone(),
        _connection_tracker: connection_tracker,
        cached_addrs: Vec::new(),
        tx_announcer: None,
        announcement_permits: AnnouncementPermits::new(),
        inbound_permits: Arc::new(Semaphore::new(inbound::INBOUND_REQUEST_CONCURRENCY)),
        overloads: Arc::new(Overloads::new()),
        outbound_permits: Arc::new(Semaphore::new(outbound::MAX_OUTBOUND_REQUEST_STREAMS)),
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

    Client {
        connection_info,
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        block_advert_tx,
        compact_receive_tx,
        inv_collector,
        error_slot,
        connection_task,
        heartbeat_task,
    }
}

/// The task that owns a connection.
struct ConnectionTask<S> {
    connection: Connection,
    /// Whether the peer dialled this node.
    is_inbound: bool,
    /// Whether this node has sent the peer its one `get-addr` request.
    sent_get_addr: bool,
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
    cached_addrs: Vec<MetaAddr>,
    /// This node's transaction announcement stream to the peer, opened on first use. Block
    /// announcements need a header the internal request does not carry, and address
    /// announcements wait for address relay.
    tx_announcer: Option<AnnouncementWriter>,
    announcement_permits: AnnouncementPermits,
    /// Bounds how many of the peer's requests are in the inbound service at once.
    inbound_permits: Arc<Semaphore>,
    /// The peer's inbound service overloads, which may close the connection.
    overloads: Arc<Overloads>,
    /// Bounds how many of this node's requests are in flight to the peer at once.
    outbound_permits: Arc<Semaphore>,
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
                // Announced addresses, and the part of a `get-addr` answer the client did not
                // get, only enter the cache, which Zebra's components drain through
                // `Request::Peers` as a bounded random sample; the peer cannot fill the address
                // book directly.
                Handler::update_addr_cache(&mut self.cached_addrs, &addrs, None);
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
            self.events.clone(),
        ));
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
            // Serve peers from the cache first, as the legacy connection does. To impede
            // address-based fingerprinting, `get-addr` is only sent on a connection this node
            // dialled, at most once (draft, "`get-addr`"); otherwise the cache is all there is,
            // even if it is empty.
            Request::Peers
                if !self.cached_addrs.is_empty() || self.is_inbound || self.sent_get_addr =>
            {
                let addrs = Handler::update_addr_cache(
                    &mut self.cached_addrs,
                    &[],
                    crate::constants::PEER_ADDR_RESPONSE_LIMIT,
                );
                Ok(Response::Peers(addrs))
            }

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
            | Request::CheckCompactBlock(_) => Err(PeerError::InboundOnlyRequest.into()),

            // Peer set internal.
            Request::PollPeerSet => Ok(Response::Nil),

            // Everything else is a request stream, driven in its own task.
            request => {
                if matches!(request, Request::Peers) {
                    self.sent_get_addr = true;
                }
                let permit = self
                    .outbound_permits
                    .clone()
                    .try_acquire_owned()
                    .expect("requests are only taken while a permit is available, and only this task takes permits");
                // The guard is created before the task is spawned, so if the task is aborted
                // before it first runs, dropping it still answers the client.
                self.tasks.spawn(outbound::drive(
                    request,
                    outbound::ResponseGuard::new(tx),
                    permit,
                    outbound::RequestContext {
                        connection: self.connection.clone(),
                        transient_addr: self.transient_addr,
                        genesis_hash: self.params.genesis_hash,
                        misbehavior: self.params.misbehavior.clone(),
                        events: self.events.clone(),
                    },
                ));
                return;
            }
        };

        let _ = tx.send(response);
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
