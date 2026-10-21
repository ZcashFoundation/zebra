//! Serving the peer's requests (draft, "Request Streams").
//!
//! Each bidirectional stream the peer opens carries one request. The task
//! here reads the stream type, waits for one of the connection's request
//! permits, then reads the request up to the bound its type allows, answers
//! it through the inbound service, and writes the response in the stream
//! type's format. Every wait is bounded by the request deadline: the type
//! byte, the permit, and the request body; and each write of the response
//! by [`REQUEST_TIMEOUT`].

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use quinn::{RecvStream, SendStream};
use tokio::sync::{mpsc, Semaphore};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::GENESIS_PREVIOUS_BLOCK_HASH,
    transaction::{Transaction, UnminedTxId},
};

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::{
        internal::{InventoryResponse, Request, Response},
        v2::{
            announcement::encode_tx_record,
            constants::{MAX_ADDRS_PER_RESPONSE, MAX_HEADERS_PER_RESPONSE},
            record::RecordCodec,
            request::{max_body_len, GetBlocks, GetHeaders, GetTx, Request as WireRequest},
            response::{encode_addr_response, write_headers_response, ObjectResult},
            types::{ErrorCode, StreamType},
            wire::{Violation, WireError},
        },
    },
    BoxError, MAX_TX_INV_IN_SENT_MESSAGE,
};

use super::{announcements::send_event, refuse_stream, Event};

/// How many of one peer's request streams may be served at once.
///
/// The legacy connection serves one request at a time per peer. Two keeps
/// the version 2 protocol's concurrency on request streams while bounding
/// how many inbound service calls they make at once.
///
/// This bounds request streams only: the announcement stream readers (up to
/// two each for block and transaction announcements) call the inbound
/// service on their own, so one peer can have up to six inbound service
/// calls outstanding. Each call holds a slot in the inbound service's shared
/// buffer only until the buffer's worker takes it, not while it is answered.
pub(super) const INBOUND_REQUEST_CONCURRENCY: usize = 2;

/// The most bytes of `get-blocks` or `get-tx` response entries one request
/// stream writes, after which the response is finished early, as the draft
/// allows (draft, "`get-blocks`", "`get-tx`": the responder SHOULD apply a
/// byte budget).
///
/// The same budget as the inbound service's legacy `getdata` limit
/// (`GETDATA_SENT_BYTES_LIMIT` in `zebrad`). It is checked before each entry,
/// so the first entry is always written, and one object larger than the
/// budget can still be sent, but only on its own.
pub(super) const RESPONSE_BYTES_LIMIT: usize = 1_000_000;

/// How many distinct transaction IDs of a `get-tx` request are looked up in
/// one inbound service call.
///
/// Chunks are requested in request order, so the transactions near the
/// start of the request, which the byte budget lets through, are looked up
/// first, and the inbound service's own byte limit rarely leaves part of a
/// chunk out.
const TX_LOOKUP_CHUNK: usize = 256;

/// How many distinct transaction IDs one `get-tx` response looks up, after
/// which the response is finished early: the inbound service's legacy
/// `getdata` limit.
const MAX_TX_LOOKUPS_PER_RESPONSE: u64 = MAX_TX_INV_IN_SENT_MESSAGE;

/// Serves one bidirectional stream the peer opened.
///
/// `get_addr_allowed` is cleared by the first `get-addr` request it lets
/// through; a `get-addr` request is refused while it is clear.
pub(super) async fn serve<S>(
    mut send: SendStream,
    mut recv: RecvStream,
    inbound_service: S,
    permits: Arc<Semaphore>,
    get_addr_allowed: Arc<AtomicBool>,
    events: mpsc::Sender<Event>,
    metrics_label: String,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    match serve_inner(
        &mut send,
        &mut recv,
        inbound_service,
        permits,
        &get_addr_allowed,
        &metrics_label,
    )
    .await
    {
        // The peer went away or cancelled the stream: nothing to do.
        Ok(()) | Err(Outcome::Transport) => {}
        Err(Outcome::Violation { reason, code }) => {
            send_event(&events, Event::Violation { reason, code }).await;
        }
        Err(Outcome::Refuse(code)) => refuse_stream(Some(&mut send), &mut recv, code),
    }
}

/// Why a request stream could not be answered normally.
enum Outcome {
    /// Close the connection with `code`: the peer violated the protocol, or
    /// (with `INTERNAL_ERROR`) this node could not encode the response.
    Violation { reason: String, code: ErrorCode },
    /// Refuse this stream and keep the connection.
    Refuse(ErrorCode),
    /// The transport ended the stream or connection.
    Transport,
}

impl From<WireError> for Outcome {
    fn from(error: WireError) -> Self {
        match error.violation() {
            Some(Violation::Close(code)) => Outcome::Violation {
                reason: error.to_string(),
                code,
            },
            // Misbehavior rules apply to responses, which a requester reads, not here.
            Some(Violation::Misbehavior) => Outcome::Refuse(ErrorCode::Refused),
            None => Outcome::Transport,
        }
    }
}

/// Reads the request, answers it, and writes the response.
async fn serve_inner<S>(
    send: &mut SendStream,
    recv: &mut RecvStream,
    inbound_service: S,
    permits: Arc<Semaphore>,
    get_addr_allowed: &AtomicBool,
    metrics_label: &str,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;

    // The stream type comes first, and decides how much request may follow.
    let mut type_byte = [0u8; 1];
    match tokio::time::timeout_at(deadline, recv.read_exact(&mut type_byte)).await {
        Ok(Ok(())) => {}
        Ok(Err(quinn::ReadExactError::FinishedEarly(_))) => {
            return Err(Outcome::Violation {
                reason: "stream finished before its type byte".to_string(),
                code: ErrorCode::ProtocolError,
            })
        }
        Ok(Err(quinn::ReadExactError::ReadError(_))) => return Err(Outcome::Transport),
        Err(_elapsed) => return Err(Outcome::Refuse(ErrorCode::Cancelled)),
    }
    let stream_type = match StreamType::from_code(type_byte[0]) {
        Some(stream_type) => stream_type,
        None => return Err(Outcome::Refuse(ErrorCode::UnsupportedStreamType)),
    };
    // Known request types this node does not serve are refused (draft, "Request Streams": the
    // responder MAY refuse). The handshake type (a second handshake stream) and the announcement
    // types (never bidirectional) are connection errors (draft, "Stream Types", "Handshake
    // Validation").
    let Some(max_len) = max_body_len(stream_type) else {
        return Err(if stream_type.is_request() {
            Outcome::Refuse(ErrorCode::Refused)
        } else {
            Outcome::Violation {
                reason: format!("{stream_type} is not a request stream type"),
                code: ErrorCode::ProtocolError,
            }
        });
    };

    // # Security
    //
    // Only a few of one peer's requests are buffered and reach the inbound service at once; the
    // rest wait here, holding nothing, until the peer's share frees up.
    let _permit = tokio::time::timeout_at(deadline, permits.acquire_owned())
        .await
        .map_err(|_elapsed| Outcome::Refuse(ErrorCode::Refused))?
        .map_err(|_closed| Outcome::Transport)?;

    // The body, up to the peer finishing its direction, bounded by what the type allows.
    let body = tokio::time::timeout_at(deadline, recv.read_to_end(max_len))
        .await
        .map_err(|_elapsed| Outcome::Refuse(ErrorCode::Cancelled))?
        .map_err(|error| match error {
            // A longer body than any conforming request: the peer is not following the format.
            quinn::ReadToEndError::TooLong => Outcome::Violation {
                reason: format!("{stream_type} request longer than the format allows"),
                code: ErrorCode::ProtocolError,
            },
            quinn::ReadToEndError::Read(_) => Outcome::Transport,
        })?;
    let wire_request = WireRequest::decode(stream_type, &body)?;

    metrics::counter!(
        "zebra.net.in.requests",
        "command" => stream_type.name(),
        "addr" => metrics_label.to_string(),
    )
    .increment(1);

    match wire_request {
        WireRequest::Headers(request) => serve_headers(send, request, inbound_service).await,
        WireRequest::Blocks(request) => serve_blocks(send, request, inbound_service).await,
        WireRequest::Tx(request) => serve_txs(send, request, inbound_service).await,
        WireRequest::Addr => {
            // To impede address-based fingerprinting, `get-addr` is only answered on a connection
            // the peer dialled, and only once (draft, "`get-addr`": reset with `REFUSED`
            // otherwise). The flag starts clear on a connection this node dialled.
            if !get_addr_allowed.swap(false, Ordering::Relaxed) {
                return Err(Outcome::Refuse(ErrorCode::Refused));
            }
            serve_addr(send, inbound_service).await
        }
        WireRequest::Mempool => serve_mempool(send, inbound_service).await,
    }?;

    metrics::counter!(
        "zebra.net.out.responses",
        "command" => stream_type.name(),
        "addr" => metrics_label.to_string(),
    )
    .increment(1);

    // `finish` only fails if the stream was already closed, which the peer may do at will.
    let _ = send.finish();
    Ok(())
}

/// Calls the inbound service, mapping its failures to stream refusals.
async fn call_service<S>(mut service: S, request: Request) -> Result<Response, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    // # Security
    //
    // The inbound service is wrapped in buffer, load-shed, and timeout layers by the caller.
    // A refused stream tells the peer to try elsewhere without ending the connection.
    if service.ready().await.is_err() {
        return Err(Outcome::Refuse(ErrorCode::Refused));
    }
    service.call(request).await.map_err(|error| {
        if error.is::<tower::load_shed::error::Overloaded>()
            || error.is::<tower::timeout::error::Elapsed>()
        {
            tracing::debug!(%error, "inbound service overloaded, refusing request stream");
        } else {
            tracing::info!(%error, "error processing peer request, refusing request stream");
        }
        Outcome::Refuse(ErrorCode::Refused)
    })
}

/// Writes `bytes` to the response stream, bounded by [`REQUEST_TIMEOUT`]: a
/// peer that opens requests and never reads the answers would otherwise pin
/// the response objects for the life of the connection.
async fn write(send: &mut SendStream, bytes: &[u8]) -> Result<(), Outcome> {
    tokio::time::timeout(REQUEST_TIMEOUT, send.write_all(bytes))
        .await
        // The peer stopped reading: abandon the response and keep the connection.
        .map_err(|_elapsed| Outcome::Refuse(ErrorCode::Cancelled))?
        .map_err(|_error| Outcome::Transport)
}

/// Serves a `get-headers` request.
///
/// Transaction IDs are not supplied yet: with `tx_ids = 1`, every entry has
/// `has_txs = 0`, and the requester falls back to `get-blocks`.
async fn serve_headers<S>(
    send: &mut SendStream,
    request: GetHeaders,
    service: S,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let mut headers: Vec<_> = if request.locator_hashes.is_empty() {
        // An empty locator asks for the single header `hash_stop` names. There is no
        // header-by-hash request, so fetch the block and keep its header.
        let response = call_service(
            service,
            Request::BlocksByHash([request.hash_stop].into_iter().collect()),
        )
        .await?;
        match response {
            Response::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(|block| match block {
                    InventoryResponse::Available((block, _)) => Some(block.header.clone()),
                    InventoryResponse::Missing(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    } else {
        let stop = (request.hash_stop != block::Hash([0; 32])).then_some(request.hash_stop);
        let response = call_service(
            service,
            Request::FindHeaders {
                known_blocks: request.locator_hashes,
                stop,
            },
        )
        .await?;
        let mut headers: Vec<_> = match response {
            Response::BlockHeaders(headers) => {
                headers.into_iter().map(|counted| counted.header).collect()
            }
            _ => Vec::new(),
        };
        // If no locator hash is in the best chain, headers start at height 1 (draft,
        // "`get-headers`"), but the state starts from the genesis block. Only the genesis header
        // has an all-zeros previous block hash, and the state only returns it when no locator
        // hash matched, so dropping it here gives the draft's start.
        if headers
            .first()
            .is_some_and(|header| header.previous_block_hash == GENESIS_PREVIOUS_BLOCK_HASH)
        {
            headers.remove(0);
        }
        headers
    };

    headers.truncate(MAX_HEADERS_PER_RESPONSE);

    let mut body = Vec::new();
    write_headers_response(&mut body, &headers, request.tx_ids)?;
    write(send, &body).await
}

/// Serves a `get-blocks` request, answering each hash in request order.
///
/// Each hash is looked up on its own, so the entries are written in request
/// order as their answers arrive. An entry is written as not found only when
/// the inbound service said so. The response is finished early, which the
/// draft allows, and the requester re-requests the remainder:
/// - at the first hash the inbound service left out of its answer, rather
///   than claim the block is missing,
/// - once [`RESPONSE_BYTES_LIMIT`] bytes of entries have been written, and
/// - when a lookup fails after some entries were written, so the requester
///   keeps them.
///
/// A hash repeated in the request is answered again from its first answer
/// without another lookup, and each copy counts against the byte budget.
async fn serve_blocks<S>(
    send: &mut SendStream,
    request: GetBlocks,
    service: S,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    // `Some(block)` is available, `None` was reported missing.
    let mut answered: HashMap<block::Hash, Option<Arc<Block>>> = HashMap::new();
    let mut sent: usize = 0;

    for hash in &request.hashes {
        if sent >= RESPONSE_BYTES_LIMIT {
            break;
        }

        if !answered.contains_key(hash) {
            let response = match call_service(
                service.clone(),
                Request::BlocksByHash([*hash].into_iter().collect()),
            )
            .await
            {
                Ok(response) => response,
                // Entries are already written: finish early so the requester keeps them.
                Err(Outcome::Refuse(code)) if sent > 0 => {
                    tracing::debug!(%code, "block lookup failed part way, finishing early");
                    break;
                }
                Err(error) => return Err(error),
            };
            if let Response::Blocks(blocks) = response {
                for block in blocks {
                    match block {
                        InventoryResponse::Available((block, _)) if block.hash() == *hash => {
                            answered.insert(*hash, Some(block));
                        }
                        InventoryResponse::Missing(missing) if missing == *hash => {
                            answered.entry(*hash).or_insert(None);
                        }
                        // Not what was asked for: ignored.
                        _ => {}
                    }
                }
            }
        }

        let entry = match answered.get(hash) {
            Some(Some(block)) => ObjectResult::Found(block.clone()),
            Some(None) => ObjectResult::NotFound,
            // Left out of the response: stop here rather than claim it is missing.
            None => break,
        };
        sent = sent.saturating_add(write_entry(send, &entry).await?);
    }
    Ok(())
}

/// Serves a `get-tx` request, answering each reference in request order.
///
/// Short ID references need compact block state this node does not keep, so
/// they are answered as not found, as the draft allows. The other references
/// are looked up in request order, [`TX_LOOKUP_CHUNK`] distinct IDs at a
/// time, and an ID the inbound service left out of a chunk's answer is
/// looked up again on its own. The response is finished early, as for
/// blocks, at the first ID the inbound service leaves out of that single
/// lookup, once [`RESPONSE_BYTES_LIMIT`] bytes of entries have been written,
/// after [`MAX_TX_LOOKUPS_PER_RESPONSE`] distinct IDs have been looked up,
/// and when a lookup fails after some entries were written, so the requester
/// keeps them. A repeated reference is answered again from its first answer,
/// and each copy counts against the byte budget.
async fn serve_txs<S>(send: &mut SendStream, request: GetTx, service: S) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    // `Some(transaction)` is available, `None` was reported missing.
    let mut answered: HashMap<UnminedTxId, Option<Arc<Transaction>>> = HashMap::new();
    // The references before this index have all been looked up, or are short IDs.
    let mut looked_up_to = 0;
    let mut sent: usize = 0;
    // Distinct IDs sent to the inbound service, including single-ID retries.
    let mut lookups: u64 = 0;

    for (index, txref) in request.txrefs.iter().enumerate() {
        if sent >= RESPONSE_BYTES_LIMIT {
            break;
        }

        let entry = match txref.unmined_id() {
            None => ObjectResult::NotFound,
            Some(id) => {
                if !answered.contains_key(&id) && index >= looked_up_to {
                    if lookups >= MAX_TX_LOOKUPS_PER_RESPONSE {
                        break;
                    }
                    // The next chunk: distinct IDs not yet answered, in request order, within the
                    // lookup limit.
                    let chunk_limit = TX_LOOKUP_CHUNK.min(
                        usize::try_from(MAX_TX_LOOKUPS_PER_RESPONSE - lookups)
                            .unwrap_or(usize::MAX),
                    );
                    let mut chunk = HashSet::new();
                    looked_up_to = index;
                    for later in &request.txrefs[index..] {
                        if chunk.len() >= chunk_limit {
                            break;
                        }
                        looked_up_to += 1;
                        if let Some(later_id) = later.unmined_id() {
                            if !answered.contains_key(&later_id) {
                                chunk.insert(later_id);
                            }
                        }
                    }
                    // `chunk` is bounded by `TX_LOOKUP_CHUNK`, so this widening is lossless.
                    lookups += chunk.len() as u64;
                    match lookup_txs(service.clone(), chunk, &mut answered).await {
                        Ok(()) => {}
                        // Entries are already written: finish early so the requester keeps them.
                        Err(Outcome::Refuse(code)) if sent > 0 => {
                            tracing::debug!(%code, "transaction lookup failed part way, finishing early");
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                if !answered.contains_key(&id) {
                    if lookups >= MAX_TX_LOOKUPS_PER_RESPONSE {
                        break;
                    }
                    lookups += 1;
                    // Left out of its chunk's answer, which the inbound service does when the
                    // chunk's transactions exceed its own byte limit: it always answers a single
                    // ID.
                    match lookup_txs(service.clone(), [id].into_iter().collect(), &mut answered)
                        .await
                    {
                        Ok(()) => {}
                        Err(Outcome::Refuse(code)) if sent > 0 => {
                            tracing::debug!(%code, "transaction lookup failed part way, finishing early");
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                match answered.get(&id) {
                    Some(Some(transaction)) => ObjectResult::Found(transaction.clone()),
                    Some(None) => ObjectResult::NotFound,
                    // Left out of the response: stop here rather than claim it is missing.
                    None => break,
                }
            }
        };
        sent = sent.saturating_add(write_entry(send, &entry).await?);
    }
    Ok(())
}

/// Looks up `ids` in the inbound service and records each answer for an ID
/// in `ids` in `answered`.
async fn lookup_txs<S>(
    service: S,
    ids: HashSet<UnminedTxId>,
    answered: &mut HashMap<UnminedTxId, Option<Arc<Transaction>>>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    if ids.is_empty() {
        return Ok(());
    }
    let response = call_service(service, Request::TransactionsById(ids.clone())).await?;
    if let Response::Transactions(transactions) = response {
        for transaction in transactions {
            match transaction {
                InventoryResponse::Available((transaction, _)) if ids.contains(&transaction.id) => {
                    answered.insert(transaction.id, Some(transaction.transaction));
                }
                InventoryResponse::Missing(id) if ids.contains(&id) => {
                    // An available copy of the same ID wins.
                    answered.entry(id).or_insert(None);
                }
                // Not what was asked for: ignored.
                _ => {}
            }
        }
    }
    Ok(())
}

/// Writes one `get-blocks` or `get-tx` response entry, and returns its
/// length in bytes.
async fn write_entry<T: zebra_chain::serialization::ZcashSerialize>(
    send: &mut SendStream,
    entry: &ObjectResult<T>,
) -> Result<usize, Outcome> {
    let mut bytes = Vec::new();
    entry.write(&mut bytes)?;
    write(send, &bytes).await?;
    Ok(bytes.len())
}

/// Serves a `get-addr` request with the addresses the inbound service
/// supplies (already sanitized by the address book).
async fn serve_addr<S>(send: &mut SendStream, service: S) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let response = call_service(service, Request::Peers).await?;
    let addrs: Vec<_> = match response {
        Response::Peers(addrs) => addrs.into_iter().take(MAX_ADDRS_PER_RESPONSE).collect(),
        _ => Vec::new(),
    };
    let body = encode_addr_response(&addrs)?;
    write(send, &body).await
}

/// Serves a `get-mempool` request with the current mempool contents, up to
/// [`MAX_TX_INV_IN_SENT_MESSAGE`] transaction IDs, the legacy limit.
///
/// The subscription is not kept open yet: after the snapshot, the stream is
/// finished, which the draft defines as ending the subscription.
async fn serve_mempool<S>(send: &mut SendStream, service: S) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let max_ids: usize = MAX_TX_INV_IN_SENT_MESSAGE
        .try_into()
        .expect("the constant is 25,000, which fits in any usize");

    let response = call_service(service, Request::MempoolTransactionIds).await?;
    if let Response::TransactionIds(ids) = response {
        // # Security
        //
        // As on the legacy connection, a snapshot this large is unlikely with the default mempool
        // config and could indicate an amplification attack, and letting peers know every
        // transaction we have isn't that important, so the IDs past the limit are dropped. The
        // inbound service relies on the network layer for this bound.
        if ids.len() > max_ids {
            tracing::debug!(
                id_count = ids.len(),
                ?MAX_TX_INV_IN_SENT_MESSAGE,
                "unusually large mempool snapshot, truncating it"
            );
        }
        for id in ids.into_iter().take(max_ids) {
            let payload = encode_tx_record(id);
            let record = RecordCodec::encode_record(&payload)?;
            write(send, &record).await?;
        }
    }
    Ok(())
}
