//! Serving the peer's requests (draft, "Request Streams").
//!
//! Each bidirectional stream the peer opens carries one request. The task
//! here reads the stream type, waits for one of the connection's request
//! permits, then reads the request up to the bound its type allows, answers
//! it through the inbound service, and writes the response in the stream
//! type's format. The whole exchange, from accepting the stream to the last
//! response byte, is bounded by one request deadline, [`REQUEST_TIMEOUT`]
//! after the stream was accepted: the type byte, the permit, the request
//! body, the inbound service calls, and every write of the response. A
//! request that misses its deadline, or whose connection is closing, is
//! abandoned: its stream is reset with `REFUSED` while no response byte has
//! been written (draft, "Request Streams": a responder that cannot or will
//! not serve a request MAY reset with `REFUSED` instead of sending a
//! response), and with `CANCELLED` once the response has begun. The draft
//! defines no code for a responder that abandons a response part way: it
//! gives `CANCELLED` to a requester that no longer wants a response, and has a
//! responder that stops after a complete entry finish its sending direction.
//! Finishing is not safe here, since a write cut off at the deadline may have
//! sent part of an entry, which the requester would read as a truncated
//! response, and `REFUSED` would tell the requester that a request it has
//! part of an answer to was not served at all, so the stream is reset with
//! `CANCELLED`, the code for a request that is no longer wanted, which is the
//! closest the draft has.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use quinn::{RecvStream, SendStream};
use tokio::sync::Semaphore;
use tower::Service;

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
    v2_address_book::V2BookHandle,
    BoxError, MAX_TX_INV_IN_SENT_MESSAGE,
};

use super::{refuse_stream, Event, Events, Inbound, Overloads, ServiceFailure};

pub(crate) mod block_range;

pub(crate) use block_range::{global_block_range_permits, BlockRangeServing};

use block_range::{serve_block_range, PendingRange};

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

/// How many inbound service calls one `get-blocks` or `get-tx` response
/// makes, after which the response is finished early.
///
/// # Security
///
/// One wire request is one legacy `getdata` message's worth of lookups, which
/// the legacy connection answers with one inbound service call. Batching
/// keeps a response to a few calls: a `get-blocks` request of
/// [`MAX_BLOCKS_PER_REQUEST`](crate::protocol::v2::constants::MAX_BLOCKS_PER_REQUEST) missing hashes takes 12, and a `get-tx` request
/// of [`MAX_TX_LOOKUPS_PER_RESPONSE`] missing IDs takes 7; the rest of the
/// limit is for lookups the inbound service answers in part.
pub(super) const MAX_LOOKUP_CALLS_PER_RESPONSE: usize = 16;

/// How many block hashes of a `get-blocks` request the first inbound service
/// call looks up; each call the inbound service answers in full doubles the
/// next one, up to [`BLOCK_LOOKUP_CHUNK`].
///
/// # Security
///
/// Blocks differ in size by three orders of magnitude, and every block a call
/// returns is held until the response ends, whether or not the byte budget
/// lets it be sent. The network layer bounds that read and memory
/// amplification on its own, whatever inbound service it serves: `zebrad`'s
/// stops reading at its own byte limit (`GETDATA_SENT_BYTES_LIMIT`) after the
/// first block of a call, but a generic inbound service may answer every
/// hash. A first call of one hash answers a request for large blocks with the
/// one block the budget allows and reads nothing else, and the doubling keeps
/// a request for small or missing blocks to a few calls. The bound is by
/// count: the sizes of a call's blocks are unknown before it is made, so a
/// request that puts small blocks before large ones can still have
/// [`BLOCK_LOOKUP_CHUNK`] blocks read for one that is sent, once per request;
/// only the inbound service can bound a call by bytes.
const FIRST_BLOCK_LOOKUP: usize = 1;

/// The most block hashes of a `get-blocks` request looked up in one inbound
/// service call: the inbound service's legacy `getdata` block limit
/// (`GETDATA_MAX_BLOCK_COUNT` in `zebrad`), above which it ignores hashes.
const BLOCK_LOOKUP_CHUNK: usize = 16;

/// How many distinct transaction IDs of a `get-tx` request the first inbound
/// service call looks up; each call the inbound service answers in full
/// doubles the next one, up to the IDs left under
/// [`MAX_TX_LOOKUPS_PER_RESPONSE`].
///
/// Chunks are requested in request order, so the transactions near the
/// start of the request, which the byte budget lets through, are looked up
/// first, and the inbound service's own byte limit rarely leaves part of a
/// small chunk out.
const TX_LOOKUP_CHUNK: usize = 256;

/// How many distinct transaction IDs one `get-tx` response looks up, after
/// which the response is finished early: the inbound service's legacy
/// `getdata` limit.
// `MAX_TX_INV_IN_SENT_MESSAGE` is 25,000, which fits in any `usize`.
const MAX_TX_LOOKUPS_PER_RESPONSE: usize = MAX_TX_INV_IN_SENT_MESSAGE as usize;

/// Serves one bidirectional stream the peer opened.
///
/// `get_addr_allowed` is cleared by the first `get-addr` request it lets
/// through; a `get-addr` request is refused while it is clear. The `get-addr`
/// answer comes from `address_book`.
///
/// A `get-block-range` request is read within the request deadline like any
/// other, but its response is bounded per chunk and in total instead (see
/// [`block_range`]).
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve<S>(
    send: SendStream,
    mut recv: RecvStream,
    inbound_service: S,
    permits: Arc<Semaphore>,
    overloads: Arc<Overloads>,
    get_addr_allowed: Arc<AtomicBool>,
    address_book: Option<V2BookHandle>,
    events: Events,
    block_ranges: BlockRangeServing,
) where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    // # Security
    //
    // One deadline for the whole exchange: a peer that reads its responses slowly, or asks for
    // many entries, cannot hold a request permit and the request's objects for longer than a
    // requester waits for a response. Dropping the exchange at the deadline releases both.
    let accepted = tokio::time::Instant::now();
    let deadline = accepted + REQUEST_TIMEOUT;
    let mut response = ResponseSend {
        send,
        written: false,
    };
    let served = tokio::time::timeout_at(
        deadline,
        serve_inner(
            &mut response,
            &mut recv,
            inbound_service.clone(),
            permits,
            &overloads,
            &get_addr_allowed,
            address_book.as_ref(),
            &block_ranges,
        ),
    )
    .await;
    let served = match served {
        Ok(Ok(Some(range))) => {
            // # Security
            //
            // A range response is too large to fit in one request deadline. It holds only its own
            // range permits, which bound how many are served at once, and is bounded per chunk and
            // in total from here.
            let inbound_service = Inbound {
                service: inbound_service,
                overloads: &overloads,
            };
            serve_block_range(
                &mut response,
                range,
                inbound_service,
                accepted + block_ranges.stream_timeout(),
                block_ranges.min_progress(),
            )
            .await
        }
        Ok(Ok(None)) => Ok(()),
        Ok(Err(outcome)) => Err(outcome),
        // The response did not complete in time: abandon the request.
        Err(_elapsed) => Err(Outcome::Refuse(response.abandon_code())),
    };
    match served {
        // The peer went away or cancelled the stream: nothing to do.
        Ok(()) | Err(Outcome::Transport) => {}
        Err(Outcome::Violation { reason, code }) => {
            events.close(Event::Violation { reason, code });
        }
        Err(Outcome::Refuse(code)) => refuse_stream(Some(&mut response.send), &mut recv, code),
        // The connection is closing: the request is abandoned like one at its deadline, then the
        // connection task closes the connection.
        Err(Outcome::Close(event)) => {
            let code = response.abandon_code();
            refuse_stream(Some(&mut response.send), &mut recv, code);
            events.close(event);
        }
    }
}

/// Why a request stream could not be answered normally.
enum Outcome {
    /// Close the connection with `code`: the peer violated the protocol, or
    /// (with `INTERNAL_ERROR`) this node could not encode the response.
    Violation { reason: String, code: ErrorCode },
    /// Refuse this stream and keep the connection.
    Refuse(ErrorCode),
    /// Refuse this stream and have the connection task act on `event`,
    /// which closes the connection without blaming the peer for a protocol
    /// violation.
    Close(Event),
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

/// Reads the request, answers it, and writes the response. The caller
/// bounds the whole exchange, every wait here included, by the request
/// deadline.
///
/// A `get-block-range` request is only read here: it is returned, with the
/// permits it was admitted with, for the caller to serve outside the request
/// deadline.
#[allow(clippy::too_many_arguments)]
async fn serve_inner<S>(
    response: &mut ResponseSend,
    recv: &mut RecvStream,
    inbound_service: S,
    permits: Arc<Semaphore>,
    overloads: &Overloads,
    get_addr_allowed: &AtomicBool,
    address_book: Option<&V2BookHandle>,
    block_ranges: &BlockRangeServing,
) -> Result<Option<PendingRange>, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    // The stream type comes first, and decides how much request may follow.
    let mut type_byte = [0u8; 1];
    match recv.read_exact(&mut type_byte).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(_)) => {
            return Err(Outcome::Violation {
                reason: "stream finished before its type byte".to_string(),
                code: ErrorCode::ProtocolError,
            })
        }
        Err(quinn::ReadExactError::ReadError(_)) => return Err(Outcome::Transport),
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
    // rest wait here, holding nothing, until the peer's share frees up. Range requests hold none
    // of these permits: they are bounded by their own, which are never waited for, so excess
    // range streams are refused at once, and never starve the peer's other requests. A range
    // stream claims its peer permit here, and the node permit only once its request is decoded,
    // so streams that never send a request cannot hold the node's permits.
    let mut range_permit = match stream_type {
        StreamType::GetBlockRange => Some(block_ranges.claim_peer()?),
        _ => None,
    };
    let _permit = match range_permit {
        Some(_) => None,
        None => Some(
            permits
                .acquire_owned()
                .await
                .map_err(|_closed| Outcome::Transport)?,
        ),
    };

    // The body, up to the peer finishing its direction, bounded by what the type allows.
    let body = recv
        .read_to_end(max_len)
        .await
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
    )
    .increment(1);

    let inbound_service = Inbound {
        service: inbound_service,
        overloads,
    };
    match wire_request {
        WireRequest::Headers(request) => serve_headers(response, request, inbound_service).await,
        WireRequest::Blocks(request) => serve_blocks(response, request, inbound_service).await,
        WireRequest::Tx(request) => serve_txs(response, request, inbound_service).await,
        WireRequest::Addr => {
            // To impede address-based fingerprinting, `get-addr` is only answered on a connection
            // the peer dialled, and only once (draft, "`get-addr`": reset with `REFUSED`
            // otherwise). The flag starts clear on a connection this node dialled.
            if !get_addr_allowed.swap(false, Ordering::Relaxed) {
                return Err(Outcome::Refuse(ErrorCode::Refused));
            }
            serve_addr(response, address_book).await
        }
        WireRequest::Mempool => serve_mempool(response, inbound_service).await,
        WireRequest::BlockRange(request) => {
            let peer = range_permit
                .take()
                .expect("claimed above, because only a get-block-range stream decodes to one");
            let permits = block_ranges.claim_node(peer)?;
            return Ok(Some(PendingRange { request, permits }));
        }
    }?;

    metrics::counter!(
        "zebra.net.out.responses",
        "command" => stream_type.name(),
    )
    .increment(1);

    // `finish` only fails if the stream was already closed, which the peer may do at will.
    let _ = response.send.finish();
    Ok(None)
}

/// Calls the inbound service, mapping its failures to stream refusals, or,
/// when the connection's overload policy drops the connection or the
/// inbound service has shut down, to a close.
async fn call_service<S>(mut inbound: Inbound<'_, S>, request: Request) -> Result<Response, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    inbound.call(request).await.map_err(|failure| match failure {
        ServiceFailure::Close(event) => Outcome::Close(event),
        // A refused stream tells the peer to try elsewhere.
        ServiceFailure::Failed(error) => {
            if error.is::<tower::load_shed::error::Overloaded>()
                || error.is::<tower::timeout::error::Elapsed>()
            {
                tracing::debug!(%error, "inbound service overloaded, refusing request stream");
            } else {
                tracing::info!(%error, "error processing peer request, refusing request stream");
            }
            Outcome::Refuse(ErrorCode::Refused)
        }
    })
}

/// This node's sending direction of a request stream, which the response is
/// written to.
struct ResponseSend {
    send: SendStream,
    /// Whether any of the response may have reached the stream, which decides
    /// the code the stream is reset with if the request is abandoned (see
    /// [`ResponseSend::abandon_code`]).
    written: bool,
}

impl ResponseSend {
    /// The code the stream is reset with if the request is abandoned now, at
    /// its deadline or because the connection is closing: `REFUSED` while none
    /// of the response has been written (draft, "Request Streams": a
    /// responder that will not serve a request MAY reset with `REFUSED`
    /// instead of sending a response), and `CANCELLED` once it has begun,
    /// which `REFUSED` would misreport as never served (see the module docs).
    fn abandon_code(&self) -> ErrorCode {
        if self.written {
            ErrorCode::Cancelled
        } else {
            ErrorCode::Refused
        }
    }

    /// Writes `bytes` to the response stream. A peer that stops reading is
    /// bounded by the request deadline in [`serve`], not here, so a slow
    /// reader cannot extend the exchange one write at a time.
    async fn write(&mut self, bytes: &[u8]) -> Result<(), Outcome> {
        // Set before the write: a write abandoned at the deadline may have sent some of its bytes.
        self.written = true;
        self.send
            .write_all(bytes)
            .await
            .map_err(|_error| Outcome::Transport)
    }
}

/// Serves a `get-headers` request.
///
/// Transaction IDs are not supplied yet: with `tx_ids = 1`, every entry has
/// `has_txs = 0`, and the requester falls back to `get-blocks`.
async fn serve_headers<S>(
    response: &mut ResponseSend,
    request: GetHeaders,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let mut headers: Vec<_> = if request.locator_hashes.is_empty() {
        // An empty locator asks for the single header `hash_stop` names. There is no
        // header-by-hash request, so fetch the block and keep its header.
        let answer = call_service(
            service,
            Request::BlocksByHash([request.hash_stop].into_iter().collect()),
        )
        .await?;
        match answer {
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
        let answer = call_service(
            service,
            Request::FindHeaders {
                known_blocks: request.locator_hashes,
                stop,
            },
        )
        .await?;
        let mut headers: Vec<_> = match answer {
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
    response.write(&body).await
}

/// Serves a `get-blocks` request, answering each hash in request order.
///
/// The hashes are looked up in request order, starting with
/// [`FIRST_BLOCK_LOOKUP`] distinct hashes per inbound service call and
/// doubling up to [`BLOCK_LOOKUP_CHUNK`] (see [`LookupCalls`] for how a call
/// the inbound service answers in part changes the next one), and the
/// entries are written in request order as their answers arrive. An entry is
/// written as not found only when the inbound service said so. The response
/// is finished early, which the draft allows, and the requester re-requests
/// the remainder:
/// - once [`RESPONSE_BYTES_LIMIT`] bytes of entries have been written,
/// - when the next hash is still unanswered after
///   [`MAX_LOOKUP_CALLS_PER_RESPONSE`] calls, rather than claim the block is
///   missing, and
/// - when a lookup fails after some entries were written, so the requester
///   keeps them.
///
/// A hash repeated in the request is answered again from its first answer
/// without another lookup, and each copy counts against the byte budget.
async fn serve_blocks<S>(
    response: &mut ResponseSend,
    request: GetBlocks,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    // `Some(block)` is available, `None` was reported missing.
    let mut answered: HashMap<block::Hash, Option<Arc<Block>>> = HashMap::new();
    let mut calls = LookupCalls::new(FIRST_BLOCK_LOOKUP);
    let mut sent: usize = 0;

    'entries: for (index, hash) in request.hashes.iter().enumerate() {
        if sent >= RESPONSE_BYTES_LIMIT {
            break;
        }

        while !answered.contains_key(hash) {
            let Some(limit) = calls.next(BLOCK_LOOKUP_CHUNK) else {
                break 'entries;
            };
            let batch = next_batch(
                request.hashes[index..].iter().copied().map(Some),
                &answered,
                limit,
            );
            match lookup_blocks(service.clone(), &batch, &mut answered).await {
                Ok(count) => calls.answered(batch.len(), count),
                // Entries are already written: finish early so the requester keeps them.
                Err(Outcome::Refuse(code)) if sent > 0 => {
                    tracing::debug!(%code, "block lookup failed part way, finishing early");
                    break 'entries;
                }
                Err(error) => return Err(error),
            }
        }

        let entry = match answered.get(hash) {
            Some(Some(block)) => ObjectResult::Found(block.clone()),
            Some(None) => ObjectResult::NotFound,
            None => unreachable!("the lookup loop only ends once the hash is answered"),
        };
        sent = sent.saturating_add(write_entry(response, &entry).await?);
    }
    Ok(())
}

/// Serves a `get-tx` request, answering each reference in request order.
///
/// Short ID references need compact block state this node does not keep, so
/// they are answered as not found, as the draft allows. The other references
/// are looked up in request order, starting with [`TX_LOOKUP_CHUNK`]
/// distinct IDs per inbound service call (see [`LookupCalls`]). The response
/// is finished early, as for blocks, when the next ID is still unanswered
/// after [`MAX_LOOKUP_CALLS_PER_RESPONSE`] calls or after
/// [`MAX_TX_LOOKUPS_PER_RESPONSE`] distinct IDs have been looked up, once
/// [`RESPONSE_BYTES_LIMIT`] bytes of entries have been written, and when a
/// lookup fails after some entries were written, so the requester keeps
/// them. A repeated reference is answered again from its first answer, and
/// each copy counts against the byte budget.
async fn serve_txs<S>(
    response: &mut ResponseSend,
    request: GetTx,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    // `Some(transaction)` is available, `None` was reported missing.
    let mut answered: HashMap<UnminedTxId, Option<Arc<Transaction>>> = HashMap::new();
    let mut calls = LookupCalls::new(TX_LOOKUP_CHUNK);
    let mut sent: usize = 0;
    // Distinct IDs sent to the inbound service, including IDs looked up again.
    let mut lookups: usize = 0;

    'entries: for (index, txref) in request.txrefs.iter().enumerate() {
        if sent >= RESPONSE_BYTES_LIMIT {
            break;
        }

        let entry = match txref.unmined_id() {
            None => ObjectResult::NotFound,
            Some(id) => {
                while !answered.contains_key(&id) {
                    let remaining = MAX_TX_LOOKUPS_PER_RESPONSE.saturating_sub(lookups);
                    let Some(limit) = calls.next(remaining) else {
                        break 'entries;
                    };
                    let batch = next_batch(
                        request.txrefs[index..]
                            .iter()
                            .map(|txref| txref.unmined_id()),
                        &answered,
                        limit,
                    );
                    lookups = lookups.saturating_add(batch.len());
                    match lookup_txs(service.clone(), &batch, &mut answered).await {
                        Ok(count) => calls.answered(batch.len(), count),
                        // Entries are already written: finish early so the requester keeps them.
                        Err(Outcome::Refuse(code)) if sent > 0 => {
                            tracing::debug!(%code, "transaction lookup failed part way, finishing early");
                            break 'entries;
                        }
                        Err(error) => return Err(error),
                    }
                }
                match answered.get(&id) {
                    Some(Some(transaction)) => ObjectResult::Found(transaction.clone()),
                    Some(None) => ObjectResult::NotFound,
                    None => unreachable!("the lookup loop only ends once the ID is answered"),
                }
            }
        };
        sent = sent.saturating_add(write_entry(response, &entry).await?);
    }
    Ok(())
}

/// The inbound service calls one response makes, and how many keys the next
/// one asks for.
///
/// A call the inbound service answers in full doubles the next call's size,
/// up to the caller's limit. A call it answers in part (an inbound service
/// with response limits leaves keys out, in no particular order) sets the
/// next call's size to the number of keys it did answer, at least one,
/// which it can be expected to answer in full; the next call starts at the
/// first unanswered key in request order, so entries keep being written.
struct LookupCalls {
    /// Calls made so far.
    made: usize,
    /// How many keys the next call asks for.
    size: usize,
}

impl LookupCalls {
    fn new(size: usize) -> Self {
        LookupCalls { made: 0, size }
    }

    /// Counts a new call and returns how many keys it asks for, at most
    /// `limit`; `None` once [`MAX_LOOKUP_CALLS_PER_RESPONSE`] calls have
    /// been made or `limit` is zero, which finishes the response.
    fn next(&mut self, limit: usize) -> Option<usize> {
        if self.made >= MAX_LOOKUP_CALLS_PER_RESPONSE || limit == 0 {
            return None;
        }
        self.made += 1;
        Some(self.size.min(limit))
    }

    /// Sizes the next call after one that asked for `asked` keys and got
    /// answers for `answered` of them.
    fn answered(&mut self, asked: usize, answered: usize) {
        self.size = if answered < asked {
            answered.max(1)
        } else {
            self.size.saturating_mul(2)
        };
    }
}

/// The next lookup: up to `limit` distinct keys from `keys`, in order,
/// skipping keys that are already answered and references without a key.
fn next_batch<K, V>(
    keys: impl IntoIterator<Item = Option<K>>,
    answered: &HashMap<K, V>,
    limit: usize,
) -> HashSet<K>
where
    K: Copy + Eq + std::hash::Hash,
{
    let mut batch = HashSet::new();
    for key in keys.into_iter().flatten() {
        if batch.len() >= limit {
            break;
        }
        if !answered.contains_key(&key) {
            batch.insert(key);
        }
    }
    batch
}

/// Looks up `hashes` in the inbound service, records each answer for a hash
/// in `hashes` in `answered`, and returns how many of `hashes` were
/// answered.
async fn lookup_blocks<S>(
    service: Inbound<'_, S>,
    hashes: &HashSet<block::Hash>,
    answered: &mut HashMap<block::Hash, Option<Arc<Block>>>,
) -> Result<usize, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    if hashes.is_empty() {
        return Ok(0);
    }
    let response = call_service(service, Request::BlocksByHash(hashes.clone())).await?;
    if let Response::Blocks(blocks) = response {
        for block in blocks {
            match block {
                InventoryResponse::Available((block, _)) if hashes.contains(&block.hash()) => {
                    answered.insert(block.hash(), Some(block));
                }
                InventoryResponse::Missing(hash) if hashes.contains(&hash) => {
                    // An available copy of the same hash wins.
                    answered.entry(hash).or_insert(None);
                }
                // Not what was asked for: ignored.
                _ => {}
            }
        }
    }
    Ok(hashes
        .iter()
        .filter(|hash| answered.contains_key(hash))
        .count())
}

/// Looks up `ids` in the inbound service, records each answer for an ID in
/// `ids` in `answered`, and returns how many of `ids` were answered.
async fn lookup_txs<S>(
    service: Inbound<'_, S>,
    ids: &HashSet<UnminedTxId>,
    answered: &mut HashMap<UnminedTxId, Option<Arc<Transaction>>>,
) -> Result<usize, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    if ids.is_empty() {
        return Ok(0);
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
    Ok(ids.iter().filter(|id| answered.contains_key(id)).count())
}

/// Writes one `get-blocks` or `get-tx` response entry, and returns its
/// length in bytes.
async fn write_entry<T: zebra_chain::serialization::ZcashSerialize>(
    response: &mut ResponseSend,
    entry: &ObjectResult<T>,
) -> Result<usize, Outcome> {
    let mut bytes = Vec::new();
    entry.write(&mut bytes)?;
    response.write(&bytes).await?;
    Ok(bytes.len())
}

/// Serves a `get-addr` request from the version 2 address segment's sample, which is
/// sanitized and only has addresses learned over version 2 (draft, "Network IDs"), or with no
/// addresses if the connection has no segment.
///
/// The inbound service is never asked: it answers from the legacy address book, whose
/// addresses may not accept QUIC.
async fn serve_addr(
    response: &mut ResponseSend,
    address_book: Option<&V2BookHandle>,
) -> Result<(), Outcome> {
    let addrs: Vec<_> = match address_book {
        Some(book) => book
            .get_addr_sample()
            .await
            .into_iter()
            .take(MAX_ADDRS_PER_RESPONSE)
            .collect(),
        None => Vec::new(),
    };
    let body = encode_addr_response(&addrs)?;
    response.write(&body).await
}

/// Serves a `get-mempool` request with the current mempool contents, up to
/// [`MAX_TX_INV_IN_SENT_MESSAGE`] transaction IDs, the legacy limit.
///
/// The subscription is not kept open yet: after the snapshot, the stream is
/// finished, which the draft defines as ending the subscription.
async fn serve_mempool<S>(
    response: &mut ResponseSend,
    service: Inbound<'_, S>,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let max_ids: usize = MAX_TX_INV_IN_SENT_MESSAGE
        .try_into()
        .expect("the constant is 25,000, which fits in any usize");

    let answer = call_service(service, Request::MempoolTransactionIds).await?;
    if let Response::TransactionIds(ids) = answer {
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
            response.write(&record).await?;
        }
    }
    Ok(())
}
