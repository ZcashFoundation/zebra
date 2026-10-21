//! Announcement streams, in both directions (draft, "Announcement
//! Streams").

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use quinn::{Connection, RecvStream};
use tokio::{
    sync::{broadcast, mpsc, Semaphore},
    task::JoinSet,
};
use tokio_util::codec::FramedRead;
use tower::{Service, ServiceExt};

use zebra_chain::transaction::UnminedTxId;

use crate::{
    peer_set::InventoryChange,
    protocol::{
        external::InventoryHash,
        internal::{Request, Response},
        v2::{
            announcement::{decode_addr_record, decode_block_announcement, decode_tx_record},
            record::RecordCodec,
            types::{ErrorCode, StreamType},
            wire::{Violation, WireError},
        },
    },
    quic::app_error_code,
    BoxError, PeerSocketAddr,
};

use super::{refuse_stream, Event};

/// How long a stream task waits to hand an event to the connection task.
pub(super) const CHANNEL_TIMEOUT: Duration = Duration::from_secs(5);

/// How many announcement batches may wait to be written before new ones are
/// dropped (draft: announcements are best-effort under backpressure).
///
/// Each batch is one internal request's worth of records, however many.
const ANNOUNCEMENT_QUEUE: usize = 16;

/// How long reading a unidirectional stream's type byte may take.
const STREAM_TYPE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most transaction announcements folded into one inbound service call.
const MAX_ANNOUNCEMENT_BATCH: usize = 1000;

/// Sends `event` to the connection task, giving up if it is gone or stuck.
pub(super) async fn send_event(events: &mpsc::Sender<Event>, event: Event) {
    let _ = tokio::time::timeout(CHANNEL_TIMEOUT, events.send(event)).await;
}

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
#[derive(Clone)]
pub(super) struct AnnouncementPermits {
    blocks: Arc<Semaphore>,
    transactions: Arc<Semaphore>,
    addresses: Arc<Semaphore>,
}

impl AnnouncementPermits {
    /// Two permits per announcement type.
    pub(super) fn new() -> Self {
        AnnouncementPermits {
            blocks: Arc::new(Semaphore::new(2)),
            transactions: Arc::new(Semaphore::new(2)),
            addresses: Arc::new(Semaphore::new(2)),
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
pub(super) async fn serve<S>(
    mut recv: RecvStream,
    permits: AnnouncementPermits,
    transient_addr: Option<PeerSocketAddr>,
    mut inbound_service: S,
    inv_collector: broadcast::Sender<InventoryChange>,
    events: mpsc::Sender<Event>,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let mut type_byte = [0u8; 1];
    let read = tokio::time::timeout(STREAM_TYPE_TIMEOUT, recv.read_exact(&mut type_byte)).await;
    match read {
        Ok(Ok(())) => {}
        Ok(Err(quinn::ReadExactError::FinishedEarly(_))) => {
            send_event(
                &events,
                Event::Violation {
                    reason: "stream finished before its type byte".to_string(),
                    code: ErrorCode::ProtocolError,
                },
            )
            .await;
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
            send_event(
                &events,
                Event::Violation {
                    reason: format!("{stream_type} stream opened as unidirectional"),
                    code: ErrorCode::ProtocolError,
                },
            )
            .await;
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
            send_event(
                &events,
                Event::Violation {
                    reason: format!("third concurrent {stream_type} announcement stream"),
                    code: ErrorCode::ProtocolError,
                },
            )
            .await;
            return;
        }
    };

    let mut records = FramedRead::new(recv, RecordCodec);
    loop {
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
                handle_block_announcements(
                    &payloads,
                    transient_addr,
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
            StreamType::AddressAnnouncements => handle_addr_announcements(&payloads, &events).await,
            _ => unreachable!("only announcement types are handed to this task"),
        };
        if let Err(code) = handled {
            send_event(
                &events,
                Event::Violation {
                    reason: format!("nonconforming {stream_type} record"),
                    code,
                },
            )
            .await;
            break;
        }
        if let Some(error) = framing_error {
            if let Some(Violation::Close(code)) = error.violation() {
                send_event(
                    &events,
                    Event::Violation {
                        reason: format!("{stream_type} stream: {error}"),
                        code,
                    },
                )
                .await;
            }
            break;
        }
    }
    // The permit is released when this task ends, however it ends.
}

/// Announcement records have no per-record refusal or misbehaviour rule:
/// every decoder error closes the connection (draft, "Announcement
/// Streams"); the fallback only keeps the match total.
fn close_code(error: WireError) -> ErrorCode {
    match error.violation() {
        Some(Violation::Close(code)) => code,
        _ => ErrorCode::ProtocolError,
    }
}

/// Acts on a batch of block announcement records.
async fn handle_block_announcements<S>(
    payloads: &[Bytes],
    transient_addr: Option<PeerSocketAddr>,
    inbound_service: &mut S,
    inv_collector: &broadcast::Sender<InventoryChange>,
) -> Result<(), ErrorCode>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    for payload in payloads {
        let header = decode_block_announcement(payload).map_err(close_code)?;
        let hash = header.hash();
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
        .await;
    }
    Ok(())
}

/// Acts on a batch of transaction announcement records with one inbound
/// service call.
async fn handle_tx_announcements<S>(
    payloads: &[Bytes],
    transient_addr: Option<PeerSocketAddr>,
    inbound_service: &mut S,
    inv_collector: &broadcast::Sender<InventoryChange>,
) -> Result<(), ErrorCode>
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
    .await;
    Ok(())
}

/// Hands a batch of address announcement records to the connection task's
/// cache; a batch with a nonconforming record contributes nothing to it.
async fn handle_addr_announcements(
    payloads: &[Bytes],
    events: &mpsc::Sender<Event>,
) -> Result<(), ErrorCode> {
    let mut addrs = Vec::new();
    for payload in payloads {
        if let Some(addr) = decode_addr_record(payload).map_err(close_code)? {
            addrs.push(addr);
        }
    }
    if !addrs.is_empty() {
        send_event(events, Event::Addresses(addrs)).await;
    }
    Ok(())
}

/// Sends an announcement-derived request to the inbound service, logging
/// failures: an announcement needs no answer, so service errors do not end
/// the connection.
async fn call_inbound<S>(inbound_service: &mut S, request: Request)
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let command = request.command();
    let result = match inbound_service.ready().await {
        Ok(service) => service.call(request).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::debug!(%error, command, "inbound service rejected announcement");
    }
}
