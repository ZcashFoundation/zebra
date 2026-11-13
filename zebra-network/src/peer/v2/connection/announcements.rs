//! Announcement streams, in both directions (draft, "Announcement
//! Streams").

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use quinn::{Connection, RecvStream};
use tokio::{
    sync::{broadcast, mpsc, Semaphore},
    task::JoinSet,
    time::Instant,
};
use tokio_util::codec::FramedRead;
use tower::Service;

use zebra_chain::{block, transaction::UnminedTxId, work::difficulty::ExpandedDifficulty};

use crate::{
    peer::v2::rate::TokenBucket,
    peer_set::InventoryChange,
    protocol::{
        external::InventoryHash,
        internal::{Request, Response},
        v2::{
            announcement::{
                decode_addr_record, decode_block_announcement, decode_tx_record,
                MAX_ADDR_RECORD_LEN, MAX_BLOCK_ANNOUNCEMENT_LEN, MAX_TX_ANNOUNCEMENT_LEN,
            },
            record::RecordCodec,
            types::{ErrorCode, StreamType},
            wire::{Violation, WireError},
        },
    },
    quic::app_error_code,
    BoxError, PeerSocketAddr,
};

use super::{
    records::RecordReader, refuse_stream, Event, Events, Inbound, Overloads, ServiceFailure,
};

/// How many announcement batches may wait to be written before new ones are
/// dropped (draft: announcements are best-effort under backpressure).
///
/// Each batch is one internal request's worth of records, however many.
const ANNOUNCEMENT_QUEUE: usize = 16;

/// How long reading a unidirectional stream's type byte may take.
const STREAM_TYPE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most transaction announcements folded into one inbound service call.
const MAX_ANNOUNCEMENT_BATCH: usize = 1000;

/// How many block announcement records a connection acts on in a row before
/// it acts on one per [`BLOCK_ANNOUNCEMENT_INTERVAL`].
///
/// A peer announces each new block once, and a few at a time after a chain
/// reorganization or when it catches up with the tip.
pub(super) const BLOCK_ANNOUNCEMENT_BURST: u32 = 16;

/// The time one block announcement's worth of a connection's rate takes to
/// refill: a fifth of the 75-second target block spacing, so a conforming
/// peer never waits.
///
/// # Security
///
/// Every block announcement record becomes an inbound service call, which
/// can start a block download. Without a rate, a peer could make this node
/// start them as fast as it writes 1.5 KiB records. Over the rate, the
/// reader stops reading the stream, which applies backpressure without
/// closing the connection (draft, "Announcement Streams": a receiver MAY
/// bound the rate at which it reads announcement records by ceasing to read
/// the stream, and SHOULD for block announcements). The peer's writes then
/// stall on flow control, and announcements are best effort, so the records
/// a peer cannot write are its to drop.
const BLOCK_ANNOUNCEMENT_INTERVAL: Duration = Duration::from_secs(15);

/// This node's announcement stream of one type to the peer, written by its
/// own task; batches are queued without waiting and dropped when the queue
/// is full (draft: best effort).
pub(super) struct AnnouncementWriter {
    queue: mpsc::Sender<Bytes>,
}

/// The writer's stream has ended; the next announcement needs a new one.
pub(super) struct WriterClosed;

impl AnnouncementWriter {
    /// Spawns the writer task for `stream_type` into `tasks`. The stream is
    /// abandoned if the peer stalls one batch for `write_timeout`.
    ///
    /// An abandoned stream is reset with `CANCELLED`, never finished: a
    /// stream finished part way through a record is a `PROTOCOL_ERROR`, and a
    /// replacement stream may only be opened once the old one is reset or
    /// finished (draft, "Records", "Announcement Streams"). A reset also
    /// reaches the peer at once, while a FIN waits behind the stalled bytes.
    pub(super) fn spawn(
        connection: Connection,
        stream_type: StreamType,
        tasks: &mut JoinSet<()>,
        write_timeout: Duration,
    ) -> Self {
        let (queue, mut batches) = mpsc::channel::<Bytes>(ANNOUNCEMENT_QUEUE);
        tasks.spawn(async move {
            // A peer that has stopped granting streams keeps this pending; the announcements
            // queued meanwhile are dropped, as the draft allows.
            let opened = tokio::time::timeout(STREAM_TYPE_TIMEOUT, connection.open_uni()).await;
            let mut send = match opened {
                Ok(Ok(send)) => send,
                Ok(Err(error)) => {
                    tracing::debug!(%error, %stream_type, "could not open announcement stream");
                    return;
                }
                Err(_elapsed) => {
                    tracing::debug!(%stream_type, "peer granted no announcement stream in time");
                    return;
                }
            };
            if let Err(error) = write_bounded(&mut send, &[stream_type.code()], write_timeout).await
            {
                tracing::debug!(%error, %stream_type, "could not start announcement stream");
                // A stream that is already closed needs no reset.
                let _ = send.reset(app_error_code(ErrorCode::Cancelled));
                return;
            }
            while let Some(batch) = batches.recv().await {
                if let Err(error) = write_bounded(&mut send, &batch, write_timeout).await {
                    tracing::debug!(%error, %stream_type, "announcement stream ended");
                    let _ = send.reset(app_error_code(ErrorCode::Cancelled));
                    return;
                }
            }
            // The connection task dropped the writer: every batch is a whole number of records,
            // so the stream ends cleanly. `finish` only fails on a stream that is already closed.
            let _ = send.finish();
        });
        AnnouncementWriter { queue }
    }

    /// Queues a batch of framed records, dropping it if the writer is
    /// behind. Reports a writer whose stream has ended, so a replacement can
    /// be opened.
    pub(super) fn announce(&self, batch: Bytes) -> Result<(), WriterClosed> {
        match self.queue.try_send(batch) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!("zebra.net.out.announcements.dropped").increment(1);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(WriterClosed),
        }
    }
}

/// Writes `bytes` to an announcement stream, giving up if the peer stalls
/// them for `write_timeout`.
async fn write_bounded(
    send: &mut quinn::SendStream,
    bytes: &[u8],
    write_timeout: Duration,
) -> Result<(), String> {
    tokio::time::timeout(write_timeout, send.write_all(bytes))
        .await
        .map_err(|_elapsed| "write stalled".to_string())?
        .map_err(|error| error.to_string())
}

/// Permits for the peer's announcement streams, two per type: the stream
/// being read plus one replacement the peer opened before the first stream's
/// records were all processed (its FIN can arrive before processing ends).
/// Both are read concurrently, so at most two readers per type, six per
/// peer, call the inbound service at once. A third concurrent stream of a
/// type waits [`STREAM_TYPE_TIMEOUT`] for a reader to finish, since this
/// node's readers may still be processing streams the peer already ended,
/// and is then the draft's `PROTOCOL_ERROR` (draft, "Announcement Streams").
///
/// The block announcement readers also share the connection's block
/// announcement rate, so a replacement stream starts with what the stream
/// it replaces left of it.
#[derive(Clone)]
pub(super) struct AnnouncementPermits {
    blocks: Arc<Semaphore>,
    transactions: Arc<Semaphore>,
    addresses: Arc<Semaphore>,
    /// Only locked to count a token, never across an await.
    block_rate: Arc<Mutex<TokenBucket>>,
}

impl AnnouncementPermits {
    /// Two permits per announcement type.
    pub(super) fn new() -> Self {
        AnnouncementPermits {
            blocks: Arc::new(Semaphore::new(2)),
            transactions: Arc::new(Semaphore::new(2)),
            addresses: Arc::new(Semaphore::new(2)),
            block_rate: Arc::new(Mutex::new(TokenBucket::new(
                BLOCK_ANNOUNCEMENT_INTERVAL,
                BLOCK_ANNOUNCEMENT_BURST,
                Instant::now(),
            ))),
        }
    }

    /// Waits until the connection's block announcement rate allows another
    /// record, and counts it.
    async fn block_announcement(&self) {
        loop {
            let next_token_at = {
                let mut block_rate = self
                    .block_rate
                    .lock()
                    .expect("no holder of the lock panics: it only counts tokens");
                let now = Instant::now();
                if block_rate.try_take(now) {
                    return;
                }
                block_rate.next_token_at(now)
            };
            tokio::time::sleep_until(next_token_at).await;
        }
    }

    fn for_type(&self, stream_type: StreamType) -> &Arc<Semaphore> {
        match stream_type {
            StreamType::BlockAnnouncements => &self.blocks,
            StreamType::TransactionAnnouncements => &self.transactions,
            StreamType::AddressAnnouncements => &self.addresses,
            _ => unreachable!("only announcement types reach the permit lookup"),
        }
    }
}

/// Reads one unidirectional stream the peer opened: its type byte, then its
/// announcement records until it ends, acting on them.
///
/// Unknown types are refused with `UNSUPPORTED_STREAM_TYPE`; known types
/// that are not announcements (which only come on bidirectional streams)
/// are a `PROTOCOL_ERROR`. Transaction and address announcements that are
/// already buffered are folded into one inbound service call or one cache
/// update, as a legacy `inv` or `addr` message would be.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve<S>(
    mut recv: RecvStream,
    permits: AnnouncementPermits,
    transient_addr: Option<PeerSocketAddr>,
    target_difficulty_limit: ExpandedDifficulty,
    inbound_service: S,
    overloads: Arc<Overloads>,
    inv_collector: broadcast::Sender<InventoryChange>,
    events: Events,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let mut type_byte = [0u8; 1];
    let read = tokio::time::timeout(STREAM_TYPE_TIMEOUT, recv.read_exact(&mut type_byte)).await;
    match read {
        Ok(Ok(())) => {}
        Ok(Err(quinn::ReadExactError::FinishedEarly(_))) => {
            events.close(Event::Violation {
                reason: "stream finished before its type byte".to_string(),
                code: ErrorCode::ProtocolError,
            });
            return;
        }
        // Reset by the peer, or the connection ended: nothing to do.
        Ok(Err(quinn::ReadExactError::ReadError(_))) => return,
        Err(_elapsed) => {
            refuse_stream(None, &mut recv, ErrorCode::Cancelled);
            return;
        }
    }

    let stream_type = match StreamType::from_code(type_byte[0]) {
        Some(stream_type) if stream_type.is_announcement() => stream_type,
        Some(stream_type) => {
            events.close(Event::Violation {
                reason: format!("{stream_type} stream opened as unidirectional"),
                code: ErrorCode::ProtocolError,
            });
            return;
        }
        None => {
            refuse_stream(None, &mut recv, ErrorCode::UnsupportedStreamType);
            return;
        }
    };

    let permit = permits.for_type(stream_type).clone().acquire_owned();
    let _permit = match tokio::time::timeout(STREAM_TYPE_TIMEOUT, permit).await {
        Ok(Ok(permit)) => permit,
        // The connection task dropped the permits: it is shutting down.
        Ok(Err(_closed)) => return,
        Err(_elapsed) => {
            events.close(Event::Violation {
                reason: format!("third concurrent {stream_type} announcement stream"),
                code: ErrorCode::ProtocolError,
            });
            return;
        }
    };

    let mut inbound_service = Inbound {
        service: inbound_service,
        overloads: &overloads,
    };
    // # Security
    //
    // Each stream is framed with the longest record of its type, so the peer cannot make this
    // node buffer more of an incomplete record than that, on any of the streams it may open.
    let max_record_len = match stream_type {
        StreamType::BlockAnnouncements => MAX_BLOCK_ANNOUNCEMENT_LEN,
        StreamType::TransactionAnnouncements => MAX_TX_ANNOUNCEMENT_LEN,
        StreamType::AddressAnnouncements => MAX_ADDR_RECORD_LEN,
        _ => unreachable!("only announcement types are handed to this task"),
    };
    let mut records = RecordReader::new(FramedRead::new(
        recv,
        RecordCodec::with_max_payload_len(max_record_len),
    ));
    loop {
        // The time spent on the last batch is not the peer's delay.
        records.pause();
        // One record, waited for; then whatever else has already arrived.
        let Some(first) = records.next().await else {
            break;
        };
        let mut batch = vec![first];
        if matches!(
            stream_type,
            StreamType::TransactionAnnouncements | StreamType::AddressAnnouncements
        ) {
            while batch.len() < MAX_ANNOUNCEMENT_BATCH {
                match records.next().now_or_never() {
                    Some(Some(record)) => batch.push(record),
                    // Nothing more buffered, or the stream ended (reported on the next loop).
                    Some(None) | None => break,
                }
            }
        }

        // Payloads up to the first framing error; the complete ones are processed first.
        let mut payloads = Vec::with_capacity(batch.len());
        let mut framing_error = None;
        for record in batch {
            match record {
                Ok(payload) => payloads.push(payload),
                Err(error) => {
                    framing_error = Some(error);
                    break;
                }
            }
        }

        let handled = match stream_type {
            StreamType::BlockAnnouncements => {
                // Nothing is read from the stream while this waits: see
                // `BLOCK_ANNOUNCEMENT_INTERVAL`.
                if !payloads.is_empty() {
                    permits.block_announcement().await;
                }
                handle_block_announcements(
                    &payloads,
                    transient_addr,
                    target_difficulty_limit,
                    &mut inbound_service,
                    &inv_collector,
                )
                .await
            }
            StreamType::TransactionAnnouncements => {
                handle_tx_announcements(
                    &payloads,
                    transient_addr,
                    &mut inbound_service,
                    &inv_collector,
                )
                .await
            }
            StreamType::AddressAnnouncements => handle_addr_announcements(&payloads, &events),
            _ => unreachable!("only announcement types are handed to this task"),
        };
        match handled {
            Ok(()) => {}
            Err(Handled::Violation(code)) => {
                events.close(Event::Violation {
                    reason: format!("nonconforming {stream_type} record"),
                    code,
                });
                break;
            }
            Err(Handled::Close(event)) => {
                events.close(event);
                break;
            }
        }
        if let Some(error) = framing_error {
            if let Some(Violation::Close(code)) = error.violation() {
                events.close(Event::Violation {
                    reason: format!("{stream_type} stream: {error}"),
                    code,
                });
            }
            break;
        }
    }
    // The permit is released when this task ends, however it ends.
}

/// Why a batch of announcement records ends the stream.
enum Handled {
    /// A nonconforming record: close the connection with this code.
    Violation(ErrorCode),
    /// The inbound service's overload policy dropped the connection: have the
    /// connection task act on this event.
    Close(Event),
}

/// Announcement records have no per-record refusal or misbehaviour rule:
/// every decoder error closes the connection (draft, "Announcement
/// Streams"); the fallback only keeps the match total.
fn close_code(error: WireError) -> Handled {
    Handled::Violation(match error.violation() {
        Some(Violation::Close(code)) => code,
        _ => ErrorCode::ProtocolError,
    })
}

/// Returns `true` if `header`, which hashes to `hash`, passes the
/// proof-of-work checks that need nothing but the header: its difficulty
/// threshold is valid and no easier than `target_difficulty_limit`, and its
/// hash meets the threshold.
///
/// These are comparisons, not the Equihash solution check, which the block's
/// verification makes. A header that fails them is not a block any node
/// accepts, so acting on its announcement would be wasted work.
fn passes_difficulty_filter(
    header: &block::Header,
    hash: block::Hash,
    target_difficulty_limit: ExpandedDifficulty,
) -> bool {
    header
        .difficulty_threshold
        .to_expanded()
        .is_some_and(|threshold| threshold <= target_difficulty_limit && hash <= threshold)
}

/// Acts on a batch of block announcement records. A header that fails
/// [`passes_difficulty_filter`] is dropped: nothing is registered or
/// forwarded for it, and the connection stays open.
async fn handle_block_announcements<S>(
    payloads: &[Bytes],
    transient_addr: Option<PeerSocketAddr>,
    target_difficulty_limit: ExpandedDifficulty,
    inbound_service: &mut Inbound<'_, S>,
    inv_collector: &broadcast::Sender<InventoryChange>,
) -> Result<(), Handled>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    for payload in payloads {
        let header = decode_block_announcement(payload).map_err(close_code)?;
        let hash = header.hash();
        if !passes_difficulty_filter(&header, hash, target_difficulty_limit) {
            tracing::debug!(
                ?hash,
                "ignoring announced header that fails the difficulty filter"
            );
            metrics::counter!("zebra.net.in.announcements.dropped").increment(1);
            continue;
        }
        if let Some(addr) = transient_addr {
            let _ = inv_collector.send(InventoryChange::new_available(
                InventoryHash::Block(hash),
                addr,
            ));
        }
        call_inbound(
            inbound_service,
            Request::AdvertiseBlock(hash, transient_addr),
        )
        .await?;
    }
    Ok(())
}

/// Acts on a batch of transaction announcement records with one inbound
/// service call.
async fn handle_tx_announcements<S>(
    payloads: &[Bytes],
    transient_addr: Option<PeerSocketAddr>,
    inbound_service: &mut Inbound<'_, S>,
    inv_collector: &broadcast::Sender<InventoryChange>,
) -> Result<(), Handled>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let mut ids: std::collections::HashSet<UnminedTxId> = std::collections::HashSet::new();
    for payload in payloads {
        let txref = decode_tx_record(payload).map_err(close_code)?;
        // `decode_tx_record` rejects short IDs, which are the only references without an ID.
        if let Some(id) = txref.unmined_id() {
            ids.insert(id);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }

    if let Some(addr) = transient_addr {
        let hashes: Vec<InventoryHash> = ids.iter().map(|id| (*id).into()).collect();
        if let Some(change) = InventoryChange::new_available_multi(hashes.iter(), addr) {
            let _ = inv_collector.send(change);
        }
    }
    call_inbound(
        inbound_service,
        Request::AdvertiseTransactionIds(ids, transient_addr),
    )
    .await
}

/// Hands a batch of address announcement records to the connection task,
/// which applies the peer's address budget, best effort; a batch with a
/// nonconforming record contributes nothing.
fn handle_addr_announcements(payloads: &[Bytes], events: &Events) -> Result<(), Handled> {
    let mut addrs = Vec::new();
    for payload in payloads {
        if let Some(addr) = decode_addr_record(payload).map_err(close_code)? {
            addrs.push(addr);
        }
    }
    if !addrs.is_empty() {
        events.addresses(addrs);
    }
    Ok(())
}

/// Sends an announcement-derived request to the inbound service, logging
/// failures: an announcement needs no answer, so service errors do not end
/// the connection, except when an overload or timeout makes the connection's
/// overload policy drop it, or the inbound service has shut down, as on the
/// legacy connection.
async fn call_inbound<S>(
    inbound_service: &mut Inbound<'_, S>,
    request: Request,
) -> Result<(), Handled>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let command = request.command();
    match inbound_service.call(request).await {
        Ok(_response) => Ok(()),
        Err(ServiceFailure::Close(event)) => Err(Handled::Close(event)),
        Err(ServiceFailure::Failed(error)) => {
            tracing::debug!(%error, command, "inbound service rejected announcement");
            Ok(())
        }
    }
}
