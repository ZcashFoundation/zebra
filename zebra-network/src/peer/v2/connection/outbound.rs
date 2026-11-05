//! Driving this node's requests to the peer (draft, "Request Streams").
//!
//! Each internal [`Request`] that needs the peer becomes one request stream
//! (or several, when the request is larger than one stream allows). The
//! task here opens the stream, writes the request, reads the response with
//! the stream type's reader, checks that each delivered object is the one
//! that was asked for, and answers the client. The client is always
//! answered, even if the task is cancelled part way, and a client that stops
//! waiting cancels the stream (draft, "Request Streams": `CANCELLED`).

use std::sync::Arc;

use quinn::{Connection, RecvStream, SendStream};
use tokio::{io::BufReader, sync::OwnedSemaphorePermit};

use zebra_chain::{
    block::{self, Block, CountedHeader},
    transaction::{Transaction, UnminedTx, UnminedTxId},
};

use crate::{
    constants::REQUEST_TIMEOUT,
    peer::{
        v2::misbehavior::MisbehaviorScores, MustUseClientResponseSender, PeerError, SharedPeerError,
    },
    protocol::{
        external::InventoryHash,
        internal::{InventoryResponse, Request, Response},
        v2::{
            constants::{
                MAX_ADDRS_PER_RESPONSE, MAX_BLOCKS_PER_REQUEST, MAX_LOCATOR_HASHES,
                MAX_TXS_PER_REQUEST, MIN_BIDIRECTIONAL_STREAMS, RESPONSE_MISBEHAVIOR_PENALTY,
            },
            request::{GetBlocks, GetHeaders, GetTx, Request as WireRequest},
            response::{decode_addr_response, read_headers, read_objects, ObjectResult},
            txref::TxRef,
            types::ErrorCode,
            wire::{Violation, WireError},
        },
    },
    quic::app_error_code,
    PeerSocketAddr,
};

use super::{close_error, Event, Events};

/// The longest `addrv2` record: time, a 9-byte services CompactSize, the
/// network ID, a 3-byte length, the 512-byte address maximum, and the port.
const MAX_ADDR_RECORD_LEN: usize = 4 + 9 + 1 + 3 + 512 + 2;

/// The longest `get-addr` response body a conforming peer can send.
const MAX_ADDR_RESPONSE_LEN: usize = 3 + MAX_ADDRS_PER_RESPONSE * MAX_ADDR_RECORD_LEN;

/// How many of this node's requests may be in flight to one peer at once,
/// each on its own request stream.
///
/// While they all are, the connection task stops taking client requests, so
/// the peer's [`Client`](crate::peer::Client) is unready, as a legacy peer is
/// while it answers its one request, and the peer set sends new requests
/// elsewhere. Small enough that a Zebra responder, which serves two of one
/// peer's requests at a time, answers the queued ones within
/// [`REQUEST_TIMEOUT`], which starts when a request is dispatched; and, with
/// the handshake stream, within the bidirectional streams any conforming peer
/// grants, so opening a request stream does not wait for stream credit
/// (draft, "Transport Requirements").
pub(super) const MAX_OUTBOUND_REQUEST_STREAMS: usize = 4;

// One stream is the handshake stream; checked at compile time. `MIN_BIDIRECTIONAL_STREAMS` is a
// `u32`, which always fits in a `usize` on supported targets. This node's own limit
// (`MAX_CONCURRENT_BIDI_STREAMS` in `quic.rs`) is checked there to be at least the draft's minimum.
const _: () = assert!(MAX_OUTBOUND_REQUEST_STREAMS < MIN_BIDIRECTIONAL_STREAMS as usize);

/// Answers the client exactly once, even if the driving task is cancelled
/// before it finishes: a dropped guard reports the connection as dropped.
struct ResponseGuard(Option<MustUseClientResponseSender>);

impl ResponseGuard {
    /// Answers the client.
    fn send(mut self, response: Result<Response, SharedPeerError>) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(response);
        }
    }
}

impl Drop for ResponseGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Err(PeerError::ConnectionDropped.into()));
        }
    }
}

/// What [`drive`] needs from the connection.
pub(super) struct RequestContext {
    /// The connection to open the request's streams on.
    pub connection: Connection,

    /// The peer's address, if it has one: delivered blocks are tagged with it,
    /// and its misbehavior is scored by it.
    pub transient_addr: Option<PeerSocketAddr>,

    /// The network's genesis block hash, for checking `get-headers` responses.
    pub genesis_hash: block::Hash,

    /// The misbehavior scores of every version 2 peer.
    pub misbehavior: MisbehaviorScores,

    /// Where to send what the connection task must act on: a close through
    /// its connection-closing queue.
    pub events: Events,
}

/// Drives one client request to completion and answers the client, holding
/// `permit`, one of the connection's [`MAX_OUTBOUND_REQUEST_STREAMS`], until
/// its streams end.
///
/// If the client stops waiting for the response, the request is abandoned:
/// its streams are cancelled with `CANCELLED` and the client is not
/// answered.
///
/// A misbehaving response adds to the peer's score in `misbehavior` before the
/// client gets its error; once the score reaches the ban threshold, the
/// connection is closed with `MISBEHAVIOR` (draft, "Misbehavior and
/// Banning").
pub(super) async fn drive(
    request: Request,
    tx: MustUseClientResponseSender,
    permit: OwnedSemaphorePermit,
    context: RequestContext,
) {
    let RequestContext {
        connection,
        transient_addr,
        genesis_hash,
        misbehavior,
        events,
    } = context;
    let mut guard = ResponseGuard(Some(tx));
    let command = request.command();
    let work = tokio::time::timeout(
        REQUEST_TIMEOUT,
        drive_inner(request, &connection, transient_addr, genesis_hash),
    );
    let result = {
        let cancelled = guard
            .0
            .as_mut()
            .expect("the guard holds the sender until it answers the client")
            .cancellation();
        tokio::select! {
            biased;
            // Dropping `work` drops its streams, which cancels them.
            () = cancelled => None,
            result = work => Some(result.unwrap_or(Err(Failure::Timeout))),
        }
    };
    let Some(result) = result else {
        metrics::counter!(
            "zebra.net.out.requests.canceled",
            "command" => command,
        )
        .increment(1);
        // The client is gone, so the sender is cancelled and is dropped without an answer.
        drop(guard.0.take());
        return;
    };
    // The request's streams ended with `work`, so the slot is free for the next request.
    drop(permit);

    match result {
        Ok(response) => {
            metrics::counter!(
                "zebra.net.in.responses",
                "command" => response.command(),
            )
            .increment(1);
            // A `get-addr` answer only comes from the connection's own request, which hands it
            // to the version 2 address segment whole: the segment's intake rules and buckets
            // bound what one peer's answer can add.
            guard.send(Ok(response));
        }
        Err(failure) => {
            tracing::debug!(%failure, command, "request to peer failed");
            let violation = match &failure {
                Failure::Violation { reason, code } => Some((reason.clone(), *code)),
                _ => None,
            };

            // Draft, "Misbehavior and Banning": a misbehaving response is discarded and penalized,
            // and the connection stays open until the peer's score reaches the ban threshold. The
            // score is kept by peer group, so it survives reconnections. It is applied before the
            // client is answered, so a caller that sees the error also sees the penalty: any ban
            // report has already been sent.
            let ban = match (&failure, transient_addr) {
                (Failure::Misbehavior(reason), Some(addr)) => misbehavior
                    .penalize(addr, RESPONSE_MISBEHAVIOR_PENALTY, reason.clone())
                    .await
                    .then(|| reason.clone()),
                _ => None,
            };

            guard.send(Err(failure.into()));
            if let Some((reason, code)) = violation {
                events.close(Event::Violation { reason, code });
            }
            if let Some(reason) = ban {
                events.close(Event::Violation {
                    reason,
                    code: ErrorCode::Misbehavior,
                });
            }
        }
    }
}

/// Why a request failed.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// The response did not complete in time.
    #[error("response timed out")]
    Timeout,

    /// The connection task closes the connection with `code`: the peer
    /// violated the protocol, or (with `INTERNAL_ERROR`) this node could not
    /// encode its request.
    #[error("closing the connection with {code}: {reason}")]
    Violation { reason: String, code: ErrorCode },

    /// The peer's response is misbehaviour: discarded, with a
    /// [`RESPONSE_MISBEHAVIOR_PENALTY`].
    #[error("misbehaving response: {0}")]
    Misbehavior(String),

    /// The peer said it has none of the requested objects: the legacy
    /// connection's `NotFoundResponse`, which the caller retries elsewhere.
    #[error("none of the requested objects were found")]
    NotFound(Vec<InventoryHash>),

    /// The peer finished the response before its first entry, which the
    /// draft allows: it is not a claim that the objects are missing, and the
    /// caller retries elsewhere.
    #[error("the peer finished the response before its first entry")]
    Unanswered,

    /// The stream or connection failed.
    #[error("transport error: {0}")]
    Transport(String),

    /// This node refuses the request without sending it. The connection task refuses these
    /// requests itself, before a stream is opened, so this is only a fallback.
    #[error("refused without sending: {0}")]
    Refused(PeerError),
}

impl From<WireError> for Failure {
    fn from(error: WireError) -> Self {
        match error.violation() {
            Some(Violation::Close(code)) => Failure::Violation {
                reason: error.to_string(),
                code,
            },
            Some(Violation::Misbehavior) => Failure::Misbehavior(error.to_string()),
            None => Failure::Transport(error.to_string()),
        }
    }
}

impl From<Failure> for SharedPeerError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Timeout => PeerError::ConnectionReceiveTimeout.into(),
            Failure::Violation { reason, code } => close_error(reason, code).into(),
            Failure::Misbehavior(reason) => PeerError::ProtocolViolation(reason).into(),
            Failure::NotFound(hashes) => PeerError::NotFoundResponse(hashes).into(),
            failure @ Failure::Unanswered => {
                PeerError::RequestStreamFailed(failure.to_string()).into()
            }
            Failure::Transport(reason) => PeerError::RequestStreamFailed(reason).into(),
            Failure::Refused(error) => error.into(),
        }
    }
}

/// Maps a stream error from quinn.
fn transport(error: impl std::fmt::Display) -> Failure {
    Failure::Transport(error.to_string())
}

/// A response stream that tells the peer to stop sending when it is dropped
/// before the response was read to its end (a timeout or a decode error): the
/// draft's `CANCELLED` code (draft, "Request Streams"). After the response
/// has been read to its FIN, quinn has already released the stream and `stop`
/// does nothing.
struct ResponseStream(RecvStream);

impl ResponseStream {
    /// The stream as a buffered reader: the response readers take result and CompactSize bytes
    /// one at a time, and unbuffered each byte would be its own `poll_read` on the stream.
    fn reader(&mut self) -> BufReader<&mut RecvStream> {
        BufReader::new(&mut self.0)
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        let _ = self.0.stop(app_error_code(ErrorCode::Cancelled));
    }
}

/// The sending half of a request stream until the request is complete: if
/// it is dropped before then (a timeout, a cancelled client, or a write
/// error), the stream is reset with `CANCELLED` rather than finished, which
/// the peer would read as a truncated request and close the connection with
/// `PROTOCOL_ERROR` (draft, "Request Streams", "Stream Types").
pub(super) struct RequestSend(pub(super) Option<SendStream>);

impl Drop for RequestSend {
    fn drop(&mut self) {
        if let Some(mut send) = self.0.take() {
            // A stream that is already closed needs no reset.
            let _ = send.reset(app_error_code(ErrorCode::Cancelled));
        }
    }
}

/// Opens a request stream, writes `request`, and finishes this node's
/// direction.
///
/// Both halves are guarded from the moment the stream opens, so a request
/// abandoned part way cancels the stream in both directions. Once finished,
/// the send half is dropped here: the queued request bytes and FIN live in
/// the connection state, and dropping a finished `SendStream` does nothing.
async fn open_request(
    connection: &Connection,
    request: &WireRequest,
) -> Result<ResponseStream, Failure> {
    let body = request.encode()?;
    let (send, recv) = connection.open_bi().await.map_err(transport)?;
    let response = ResponseStream(recv);
    let mut guard = RequestSend(Some(send));
    let send = guard
        .0
        .as_mut()
        .expect("the guard holds the stream until it is finished");
    send.write_all(&[request.stream_type().code()])
        .await
        .map_err(transport)?;
    send.write_all(&body).await.map_err(transport)?;
    send.finish().map_err(transport)?;
    // Finished: nothing is left for the guard to cancel.
    guard.0 = None;
    Ok(response)
}

/// Drives `request` without the timeout or the client reply.
async fn drive_inner(
    request: Request,
    connection: &Connection,
    transient_addr: Option<PeerSocketAddr>,
    genesis_hash: block::Hash,
) -> Result<Response, Failure> {
    let response = match request {
        Request::Peers => {
            let mut recv = open_request(connection, &WireRequest::Addr).await?;
            let body =
                recv.0
                    .read_to_end(MAX_ADDR_RESPONSE_LEN)
                    .await
                    .map_err(|error| match error {
                        quinn::ReadToEndError::TooLong => Failure::Violation {
                            reason: "get-addr response longer than any conforming response"
                                .to_string(),
                            code: ErrorCode::ProtocolError,
                        },
                        quinn::ReadToEndError::Read(error) => transport(error),
                    })?;
            Response::Peers(decode_addr_response(&body)?)
        }

        Request::BlocksByHash(hashes) => {
            let hashes: Vec<block::Hash> = hashes.into_iter().collect();
            let mut results = Vec::with_capacity(hashes.len());
            for chunk in hashes.chunks(MAX_BLOCKS_PER_REQUEST) {
                results.extend(fetch_blocks(connection, chunk, transient_addr).await?);
            }
            Response::Blocks(inventory_results(results, &hashes)?)
        }
        Request::BlockDownload { hash, .. } => {
            let results = fetch_blocks(connection, &[hash], transient_addr).await?;
            Response::Blocks(inventory_results(results, &[hash])?)
        }

        Request::TransactionsById(ids) => {
            let ids: Vec<UnminedTxId> = ids.into_iter().collect();
            let mut results = Vec::with_capacity(ids.len());
            for chunk in ids.chunks(MAX_TXS_PER_REQUEST) {
                results.extend(fetch_transactions(connection, chunk, transient_addr).await?);
            }
            Response::Transactions(inventory_results(results, &ids)?)
        }

        Request::FindHeaders { known_blocks, stop } => {
            let headers = fetch_headers(connection, known_blocks, stop, genesis_hash).await?;
            Response::BlockHeaders(
                headers
                    .into_iter()
                    .map(|header| CountedHeader { header })
                    .collect(),
            )
        }
        // There is no `get-hashes` yet: headers give the same chain walk, 160 at a time.
        Request::FindBlocks { known_blocks, stop } => {
            let headers = fetch_headers(connection, known_blocks, stop, genesis_hash).await?;
            Response::BlockHashes(headers.iter().map(|header| header.hash()).collect())
        }

        // The subscription has no snapshot delimiter, so there is nothing to answer with yet;
        // the peer set routes this request to legacy peers.
        Request::MempoolTransactionIds => {
            tracing::debug!("get-mempool is not used on v2 connections yet");
            Response::TransactionIds(Vec::new())
        }

        // Handled by the connection task without a stream.
        Request::Ping(_)
        | Request::PushTransaction(..)
        | Request::AdvertiseTransactionIds(..)
        | Request::PollPeerSet => Response::Nil,

        // Refused by the connection task, which never sends them to the peer.
        Request::AdvertiseBlock(..)
        | Request::AdvertiseBlockToAll(_)
        | Request::AdvertiseEarlyBlock(_) => {
            return Err(Failure::Refused(PeerError::AdvertRequiresDedicatedService))
        }
        Request::EarlyBlocksByHash(_)
        | Request::AdvertiseCompactBlock(..)
        | Request::CheckCompactHeader(_)
        | Request::CompactBlockTransactions(_)
        | Request::CheckCompactBlock(_) => {
            return Err(Failure::Refused(PeerError::InboundOnlyRequest))
        }
    };
    Ok(response)
}

/// Fetches up to one request's worth of blocks, checking each delivered
/// block against the hash it answers.
///
/// A responder may finish early; the hashes it did not answer are left out
/// of the result rather than reported as missing, since the peer may well
/// have them. Only an explicit not-found entry is missing.
async fn fetch_blocks(
    connection: &Connection,
    hashes: &[block::Hash],
    transient_addr: Option<PeerSocketAddr>,
) -> Result<Vec<InventoryResponse<(Arc<Block>, Option<PeerSocketAddr>), block::Hash>>, Failure> {
    let request = WireRequest::Blocks(GetBlocks {
        hashes: hashes.to_vec(),
    });
    let mut recv = open_request(connection, &request).await?;
    let entries = read_objects::<_, Block>(&mut recv.reader(), hashes.len()).await?;

    let mut results = Vec::with_capacity(entries.len());
    for (entry, &hash) in entries.into_iter().zip(hashes) {
        match entry {
            ObjectResult::Found(block) => {
                // Draft, "get-blocks": a block that does not hash to its entry's hash is a
                // PROTOCOL_ERROR, checked by hashing alone.
                if block.hash() != hash {
                    return Err(Failure::Violation {
                        reason: format!(
                            "get-blocks entry for {hash} delivered block {}",
                            block.hash()
                        ),
                        code: ErrorCode::ProtocolError,
                    });
                }
                results.push(InventoryResponse::Available((block, transient_addr)));
            }
            ObjectResult::NotFound => results.push(InventoryResponse::Missing(hash)),
        }
    }
    Ok(results)
}

/// Fetches up to one request's worth of transactions, checking each
/// delivered transaction against its reference. Unanswered references are
/// left out, as for blocks.
async fn fetch_transactions(
    connection: &Connection,
    ids: &[UnminedTxId],
    transient_addr: Option<PeerSocketAddr>,
) -> Result<Vec<InventoryResponse<(UnminedTx, Option<PeerSocketAddr>), UnminedTxId>>, Failure> {
    let request = WireRequest::Tx(GetTx {
        txrefs: ids.iter().copied().map(TxRef::from).collect(),
    });
    let mut recv = open_request(connection, &request).await?;
    let entries = read_objects::<_, Transaction>(&mut recv.reader(), ids.len()).await?;

    let mut results = Vec::with_capacity(entries.len());
    for (entry, &id) in entries.into_iter().zip(ids) {
        match entry {
            ObjectResult::Found(transaction) => {
                let unmined = UnminedTx::from(transaction);
                // Draft, "get-tx": the transaction must match the reference's identifier.
                if !matches_reference(&unmined, id) {
                    return Err(Failure::Violation {
                        reason: format!("get-tx entry for {id:?} delivered {:?}", unmined.id),
                        code: ErrorCode::ProtocolError,
                    });
                }
                results.push(InventoryResponse::Available((unmined, transient_addr)));
            }
            ObjectResult::NotFound => results.push(InventoryResponse::Missing(id)),
        }
    }
    Ok(results)
}

/// Checks the entries of every stream of an inventory request, which asked
/// for the distinct `requested` objects, before they are handed to the
/// client.
///
/// As in the legacy connection, a response with nothing found is an error the
/// caller retries elsewhere, not a response its callers do not expect: a
/// single-object download treats an empty or all-missing response as a bug.
/// - Every requested object explicitly not found: the legacy
///   `NotFoundResponse`, which registers them all as missing at this peer.
/// - No entries at all: the peer finished early, which says nothing about
///   what it has, so the error does not claim anything is missing.
/// - Otherwise the entries are the response, with the objects the peer left
///   unanswered left out, and only explicit not-found entries missing.
fn inventory_results<A, M>(
    results: Vec<InventoryResponse<A, M>>,
    requested: &[M],
) -> Result<Vec<InventoryResponse<A, M>>, Failure>
where
    M: Copy + Into<InventoryHash>,
{
    if results.is_empty() {
        return Err(Failure::Unanswered);
    }
    let all_missing = results
        .iter()
        .all(|result| matches!(result, InventoryResponse::Missing(_)));
    if all_missing && results.len() == requested.len() {
        return Err(Failure::NotFound(
            requested.iter().copied().map(Into::into).collect(),
        ));
    }
    Ok(results)
}

/// Whether a delivered transaction matches the identifier it was requested
/// by: txid for a legacy reference, wtxid for a witnessed one.
fn matches_reference(transaction: &UnminedTx, requested: UnminedTxId) -> bool {
    match requested {
        UnminedTxId::Legacy(hash) => transaction.id.mined_id() == hash,
        UnminedTxId::Witnessed(wtx_id) => transaction.id == UnminedTxId::Witnessed(wtx_id),
    }
}

/// Fetches headers after the first known block, as `get-headers` without
/// transaction IDs.
///
/// `genesis_hash` is the network's genesis block hash: a peer that knows none
/// of the locator hashes answers from height 1, after the genesis block. If
/// the locator does not include the genesis block, such a response is
/// discarded without a penalty, and this returns no headers (see
/// [`HeadersCheck::NotAfterLocator`]).
async fn fetch_headers(
    connection: &Connection,
    known_blocks: Vec<block::Hash>,
    stop: Option<block::Hash>,
    genesis_hash: block::Hash,
) -> Result<Vec<Arc<block::Header>>, Failure> {
    let mut locator_hashes = known_blocks;
    locator_hashes.truncate(MAX_LOCATOR_HASHES);
    let request = GetHeaders {
        locator_hashes,
        hash_stop: stop.unwrap_or(block::Hash([0; 32])),
        // `read_headers` below decodes only the `tx_ids = 0` layout.
        tx_ids: false,
    };
    let wire_request = WireRequest::Headers(request.clone());
    let mut recv = open_request(connection, &wire_request).await?;
    let headers = read_headers(&mut recv.reader()).await?;

    match check_headers_response(&request, &headers, genesis_hash) {
        HeadersCheck::Selectable => Ok(headers),
        HeadersCheck::NotAfterLocator => {
            tracing::debug!(
                headers = headers.len(),
                "discarding a get-headers response that starts after the genesis block, which \
                 the locator does not include",
            );
            Ok(Vec::new())
        }
        HeadersCheck::Unselectable(reason) => Err(Failure::Misbehavior(reason)),
    }
}

/// Whether a `get-headers` response is one its request could have selected.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeadersCheck {
    /// The response follows from the request.
    Selectable,

    /// The response starts after the genesis block, which the locator does
    /// not include.
    ///
    /// A peer that knows none of the locator hashes answers this way (draft,
    /// "`get-headers`"), so the response is not misbehavior. But it does not
    /// connect to anything this node asked about: Zebra's locator always
    /// includes the genesis block while its tip is within
    /// `MAX_BLOCK_REORG_HEIGHT` of it, so a peer that shares none of the
    /// locator's blocks is either behind this node, or on a fork deeper than
    /// Zebra can reorganize to. Its headers are discarded without a penalty,
    /// so a peer cannot use this answer to inject an unrelated chain into
    /// this node's sync.
    NotAfterLocator,

    /// The response is misbehavior, for this reason.
    Unselectable(String),
}

/// Checks that `headers` is a response `request` could have selected (draft,
/// "`get-headers`").
///
/// Every check depends only on the request and the response, never on this
/// node's chain, so a peer on a different best chain passes them all: it
/// starts after the first locator hash in its best chain, or after the genesis
/// block if it has none of them. A response after a genesis block that the
/// locator does not include is valid but is not used: see
/// [`HeadersCheck::NotAfterLocator`].
fn check_headers_response(
    request: &GetHeaders,
    headers: &[Arc<block::Header>],
    genesis_hash: block::Hash,
) -> HeadersCheck {
    match unselectable_reason(request, headers, genesis_hash) {
        Some(reason) => HeadersCheck::Unselectable(reason),
        None if headers.first().is_some_and(|first| {
            !request.locator_hashes.is_empty()
                && !request.locator_hashes.contains(&first.previous_block_hash)
        }) =>
        {
            // The only other start `unselectable_reason` accepts is after the genesis block.
            HeadersCheck::NotAfterLocator
        }
        None => HeadersCheck::Selectable,
    }
}

/// Returns why `request` could not have selected `headers`, or `None` if it
/// could have.
fn unselectable_reason(
    request: &GetHeaders,
    headers: &[Arc<block::Header>],
    genesis_hash: block::Hash,
) -> Option<String> {
    let first = headers.first()?;
    let hashes: Vec<block::Hash> = headers.iter().map(|header| header.hash()).collect();

    // The headers must form a contiguous chain.
    if headers
        .iter()
        .skip(1)
        .zip(&hashes)
        .any(|(header, previous_hash)| header.previous_block_hash != *previous_hash)
    {
        return Some("get-headers response is not a contiguous chain".to_string());
    }

    if request.locator_hashes.is_empty() {
        // An empty locator selects only the header `hash_stop` names.
        if hashes != [request.hash_stop] {
            return Some(format!(
                "get-headers response to an empty locator is not the single header {}",
                request.hash_stop,
            ));
        }
        return None;
    }

    // The response starts after one of the locator hashes, or after the genesis block.
    if first.previous_block_hash != genesis_hash
        && !request.locator_hashes.contains(&first.previous_block_hash)
    {
        return Some(format!(
            "get-headers response starts after {}, which is not a locator hash or the genesis \
             block",
            first.previous_block_hash,
        ));
    }

    // The response ends at `hash_stop`, if it reaches it. An all-zeros `hash_stop` is not the
    // hash of any header, so it never matches.
    if let Some(index) = hashes.iter().position(|hash| *hash == request.hash_stop) {
        if index + 1 != hashes.len() {
            return Some(format!(
                "get-headers response continues past hash_stop {}",
                request.hash_stop,
            ));
        }
    }

    None
}
