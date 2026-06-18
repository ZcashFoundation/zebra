//! Legacy Zebra gossip compatibility over Zakura streams.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    future::Future,
    io::Cursor,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex, OnceLock,
    },
    task::{Context, Poll},
    time::Duration,
};

use serde_json::{Map, Number, Value};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, Semaphore},
    time::{sleep, timeout, Instant},
};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, MAX_BLOCK_LOCATOR_LENGTH},
    serialization::{
        CompactSizeMessage, SerializationError, ZcashDeserialize, ZcashSerialize,
        MAX_HEADERS_PER_MESSAGE, MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::{Transaction, UnminedTx, UnminedTxId},
};

use crate::{
    protocol::{
        external::InventoryHash,
        internal::{InventoryResponse, PeerSource, Request, Response},
    },
    BoxError, MAX_TX_INV_IN_SENT_MESSAGE,
};

use super::{
    spawn_supervised_peer_task, trace::peer_label as trace_peer_label, BoxRunFuture, Frame,
    FramedSend, OrderedSendError, Peer, RequestResponseService, Service as ZakuraService,
    SinkReject, Stream, StreamMode, ZakuraPeerHandle, ZakuraPeerId, ZakuraSupervisorHandle,
    ZakuraTrace, FRAME_HEADER_BYTES, LEGACY_REQUEST_TABLE, LOCAL_MAX_CONTROL_FRAME_BYTES,
    ZAKURA_CAP_LEGACY_GOSSIP,
};

/// Zakura stream kind reserved for legacy gossip compatibility.
pub const ZAKURA_STREAM_GOSSIP: u16 = 2;
/// Zakura stream kind reserved for legacy inventory request/response compatibility.
pub const ZAKURA_STREAM_LEGACY_REQUESTS: u16 = 3;
/// Version of the legacy gossip compatibility stream.
pub const LEGACY_GOSSIP_VERSION: u16 = 1;
/// A block hash advertisement.
pub const MSG_ADVERTISE_BLOCK: u16 = 1;
/// A transaction-id advertisement.
pub const MSG_ADVERTISE_TX_IDS: u16 = 2;
/// Request block contents by hash.
pub const MSG_REQUEST_BLOCKS_BY_HASH: u16 = 3;
/// Request transaction contents by unmined transaction id.
pub const MSG_REQUEST_TRANSACTIONS_BY_ID: u16 = 4;
/// A chunk of one available block response.
pub const MSG_RESPONSE_BLOCK: u16 = 5;
/// A chunk of one available transaction response.
pub const MSG_RESPONSE_TRANSACTION: u16 = 6;
/// Missing block hashes.
pub const MSG_RESPONSE_MISSING_BLOCKS: u16 = 7;
/// Missing transaction ids.
pub const MSG_RESPONSE_MISSING_TRANSACTIONS: u16 = 8;
/// Request subsequent block hashes from a block locator.
pub const MSG_REQUEST_FIND_BLOCKS: u16 = 9;
/// Request subsequent block headers from a block locator.
pub const MSG_REQUEST_FIND_HEADERS: u16 = 10;
/// Request mempool transaction IDs.
pub const MSG_REQUEST_MEMPOOL_TRANSACTION_IDS: u16 = 11;
/// Ping a peer over the request/response stream.
pub const MSG_REQUEST_PING: u16 = 12;
/// Push an unsolicited transaction to a peer.
pub const MSG_REQUEST_PUSH_TRANSACTION: u16 = 13;
/// Subsequent block hashes response.
pub const MSG_RESPONSE_BLOCK_HASHES: u16 = 14;
/// Subsequent block headers response.
pub const MSG_RESPONSE_BLOCK_HEADERS: u16 = 15;
/// Mempool transaction IDs response.
pub const MSG_RESPONSE_TRANSACTION_IDS: u16 = 16;
/// Pong response for a ping request.
pub const MSG_RESPONSE_PONG: u16 = 17;
/// Nil response for fire-and-forget legacy requests.
pub const MSG_RESPONSE_NIL: u16 = 18;

const LEGACY_GOSSIP_INBOUND_QUEUE: usize = 256;
const LEGACY_REQUEST_IN_FLIGHT_LIMIT: usize = 64;
const LEGACY_GOSSIP_SERVICE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_FIRST_SEEN_TTL: Duration = Duration::from_secs(10 * 60);
const DEFAULT_FIRST_SEEN_CAPACITY: usize = 50_000;
/// A failed gossip inbound attempt that consumed at least this long is treated as
/// "expensive" (a slow/backpressured/timed-out service), and the same inventory is
/// placed in a short cooldown. The first-seen cache is only updated after a
/// *successful* call, so without this an authenticated peer could replay the same
/// valid advertisement while the inbound service is slow/erroring and make the
/// serial gossip worker re-pay the full 30s readiness/call budget for every
/// duplicate. Fast failures stay below this threshold and remain immediately
/// retryable, so a transient blip does not drop the advertisement.
const LEGACY_GOSSIP_EXPENSIVE_ATTEMPT: Duration = Duration::from_secs(1);
/// How long an expensive failed attempt suppresses duplicate copies of the same
/// inventory before a genuine re-advertisement may retry.
const LEGACY_GOSSIP_DUPLICATE_COOLDOWN: Duration = Duration::from_secs(30);
const LEGACY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SOURCE_INVENTORY_MISSING_RETRIES: usize = 8;
const SOURCE_INVENTORY_MISSING_RETRY_DELAY: Duration = Duration::from_millis(500);
const LEGACY_REQUEST_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the dual-stack tries the (buffered) legacy peer set for an inventory
/// fetch before falling back to Zakura. Without this bound, a node that upgraded
/// all its peers to Zakura (and so has no ready legacy peer) would block every
/// fetch on the legacy peer set forever, starving the Zakura path.
const DUAL_STACK_LEGACY_INVENTORY_TIMEOUT: Duration = Duration::from_secs(3);
const LEGACY_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;
/// Maximum cumulative response payload bytes the inbound responder will buffer
/// for a single legacy request before aborting.
///
/// A peer can name up to `MAX_TX_INV_IN_SENT_MESSAGE` block/transaction hashes
/// on one request; without an aggregate cap `encode_response` would serialize
/// and retain the entire multi-frame `Vec<Frame>` (worst case
/// `MAX_TX_INV_IN_SENT_MESSAGE * MAX_PROTOCOL_MESSAGE_LEN`, tens of GiB) before
/// the first byte is written. The outbound reader already enforces a symmetric
/// per-response cap (`LegacyResponseBudget`); this is the responder-side mirror.
/// Sized well above any single response the local service emits (zebrad caps
/// `getdata` at ~1 MiB) yet far below memory-risk thresholds.
const LEGACY_RESPONSE_MAX_AGGREGATE_BYTES: usize = 8 * MAX_PROTOCOL_MESSAGE_LEN;
const REQUEST_ID_BYTES: usize = 8;
const RESPONSE_CHUNK_HEADER_BYTES: usize = REQUEST_ID_BYTES + 1;
const NO_STOP_HASH: block::Hash = block::Hash([0; 32]);
const LEGACY_GOSSIP_SERVICE_STREAMS: [Stream; 2] = [
    Stream {
        kind: ZAKURA_STREAM_GOSSIP,
        version: LEGACY_GOSSIP_VERSION,
        // Advisory until the transport wires Stream::frame_cap end-to-end; the
        // authoritative inbound cap is app_frame_cap_for_stream_kind.
        frame_cap: LOCAL_MAX_CONTROL_FRAME_BYTES,
        capability: ZAKURA_CAP_LEGACY_GOSSIP,
        mode: StreamMode::Ordered,
    },
    Stream {
        kind: ZAKURA_STREAM_LEGACY_REQUESTS,
        version: LEGACY_GOSSIP_VERSION,
        // Advisory until the transport wires Stream::frame_cap end-to-end; the
        // authoritative inbound cap is app_frame_cap_for_stream_kind.
        frame_cap: LOCAL_MAX_CONTROL_FRAME_BYTES,
        capability: ZAKURA_CAP_LEGACY_GOSSIP,
        mode: StreamMode::RequestResponse,
    },
];

/// Service-declared streams for legacy gossip compatibility.
pub(crate) fn legacy_gossip_streams() -> &'static [Stream] {
    &LEGACY_GOSSIP_SERVICE_STREAMS
}

static FIRST_SEEN_BY_SUPERVISOR: OnceLock<std::sync::Mutex<HashMap<u64, FirstSeenCache>>> =
    OnceLock::new();
static NEXT_LEGACY_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// A typed legacy gossip frame carried by Zakura stream kind 2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyGossipFrame {
    /// Advertise one block hash.
    AdvertiseBlock(block::Hash),
    /// Advertise one or more unmined transaction IDs.
    AdvertiseTransactionIds(Vec<UnminedTxId>),
}

impl LegacyGossipFrame {
    /// Convert a legacy network request into a gossip frame.
    pub fn from_request(request: Request) -> Result<Self, LegacyGossipError> {
        match request {
            Request::AdvertiseBlock(hash, _) | Request::AdvertiseBlockToAll(hash) => {
                Ok(Self::AdvertiseBlock(hash))
            }
            Request::AdvertiseTransactionIds(ids, _) => Self::advertise_transaction_ids(ids),
            request => Err(LegacyGossipError::UnsupportedRequest(request.command())),
        }
    }

    /// Build a tx-id gossip frame, enforcing the outbound non-empty/cap invariant.
    pub fn advertise_transaction_ids(
        ids: impl IntoIterator<Item = UnminedTxId>,
    ) -> Result<Self, LegacyGossipError> {
        let ids = collect_outbound_tx_ids(ids)?;
        Ok(Self::AdvertiseTransactionIds(ids))
    }

    /// Convert this typed frame to a Zakura wire frame.
    pub fn encode_frame(&self) -> Result<Frame, LegacyGossipError> {
        match self {
            Self::AdvertiseBlock(hash) => {
                let mut payload = Vec::new();
                hash.zcash_serialize(&mut payload)?;
                Ok(Frame {
                    message_type: MSG_ADVERTISE_BLOCK,
                    flags: 0,
                    payload,
                })
            }
            Self::AdvertiseTransactionIds(ids) => {
                let ids = outbound_tx_id_slice(ids)?;
                let mut payload = Vec::new();
                write_tx_id_list(&mut payload, ids)?;
                Ok(Frame {
                    message_type: MSG_ADVERTISE_TX_IDS,
                    flags: 0,
                    payload,
                })
            }
        }
    }

    /// Decode a Zakura frame into a typed gossip frame.
    pub fn decode_frame(frame: Frame) -> Result<Self, LegacyGossipError> {
        if frame.flags != 0 {
            return Err(LegacyGossipError::UnsupportedFlags(frame.flags));
        }

        match frame.message_type {
            MSG_ADVERTISE_BLOCK => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let hash = block::Hash::zcash_deserialize(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::AdvertiseBlock(hash))
            }
            MSG_ADVERTISE_TX_IDS => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let ids = read_tx_id_list(&mut reader)?;
                if ids.is_empty() {
                    return Err(LegacyGossipError::EmptyTransactionAdvertisement);
                }
                reject_trailing(&reader)?;
                Ok(Self::AdvertiseTransactionIds(ids))
            }
            message_type => Err(LegacyGossipError::UnknownMessageType(message_type)),
        }
    }

    fn into_request(self, peer_id: ZakuraPeerId) -> Request {
        match self {
            Self::AdvertiseBlock(hash) => {
                Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(peer_id)))
            }
            Self::AdvertiseTransactionIds(ids) => Request::AdvertiseTransactionIds(
                ids.into_iter().collect(),
                Some(PeerSource::Zakura(peer_id)),
            ),
        }
    }
}

/// A typed legacy inventory request carried by Zakura stream kind 3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyRequestFrame {
    /// Request block contents by hash.
    BlocksByHash(Vec<block::Hash>),
    /// Request transaction contents by unmined transaction id.
    TransactionsById(Vec<UnminedTxId>),
    /// Request block hashes after a locator.
    FindBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optional stop hash.
        stop: Option<block::Hash>,
    },
    /// Request block headers after a locator.
    FindHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optional stop hash.
        stop: Option<block::Hash>,
    },
    /// Request mempool transaction IDs.
    MempoolTransactionIds,
    /// Ping a peer.
    Ping,
    /// Push an unsolicited transaction.
    PushTransaction(UnminedTx),
}

impl LegacyRequestFrame {
    /// Convert a legacy network request into an inventory request frame.
    pub fn from_request(request: Request) -> Result<Self, LegacyGossipError> {
        match request {
            Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                let hashes = truncate_to_inventory_cap(hashes)?;
                Ok(Self::BlocksByHash(hashes))
            }
            Request::TransactionsById(ids) | Request::TransactionsByIdFrom { ids, .. } => {
                let ids = truncate_to_inventory_cap(ids)?;
                Ok(Self::TransactionsById(ids))
            }
            Request::FindBlocks { known_blocks, stop } => {
                ensure_block_locator_count(known_blocks.len())?;
                Ok(Self::FindBlocks { known_blocks, stop })
            }
            Request::FindHeaders { known_blocks, stop } => {
                ensure_block_locator_count(known_blocks.len())?;
                Ok(Self::FindHeaders { known_blocks, stop })
            }
            Request::MempoolTransactionIds => Ok(Self::MempoolTransactionIds),
            Request::Ping(_) => Ok(Self::Ping),
            Request::PushTransaction(transaction) => Ok(Self::PushTransaction(transaction)),
            request => Err(LegacyGossipError::UnsupportedRequest(request.command())),
        }
    }

    /// Convert this typed request to a Zakura wire frame.
    pub fn encode_frame(&self) -> Result<Frame, LegacyGossipError> {
        match self {
            Self::BlocksByHash(hashes) => {
                let mut payload = Vec::new();
                write_hash_list(&mut payload, hashes)?;
                Ok(Frame {
                    message_type: MSG_REQUEST_BLOCKS_BY_HASH,
                    flags: 0,
                    payload,
                })
            }
            Self::TransactionsById(ids) => {
                let mut payload = Vec::new();
                write_tx_id_list(&mut payload, ids)?;
                Ok(Frame {
                    message_type: MSG_REQUEST_TRANSACTIONS_BY_ID,
                    flags: 0,
                    payload,
                })
            }
            Self::FindBlocks { known_blocks, stop } => {
                let mut payload = Vec::new();
                write_block_locator(&mut payload, known_blocks, *stop)?;
                Ok(Frame {
                    message_type: MSG_REQUEST_FIND_BLOCKS,
                    flags: 0,
                    payload,
                })
            }
            Self::FindHeaders { known_blocks, stop } => {
                let mut payload = Vec::new();
                write_block_locator(&mut payload, known_blocks, *stop)?;
                Ok(Frame {
                    message_type: MSG_REQUEST_FIND_HEADERS,
                    flags: 0,
                    payload,
                })
            }
            Self::MempoolTransactionIds => Ok(Frame {
                message_type: MSG_REQUEST_MEMPOOL_TRANSACTION_IDS,
                flags: 0,
                payload: Vec::new(),
            }),
            Self::Ping => Ok(Frame {
                message_type: MSG_REQUEST_PING,
                flags: 0,
                payload: Vec::new(),
            }),
            Self::PushTransaction(transaction) => Ok(Frame {
                message_type: MSG_REQUEST_PUSH_TRANSACTION,
                flags: 0,
                payload: transaction.transaction.zcash_serialize_to_vec()?,
            }),
        }
    }

    /// Decode a Zakura frame into a typed inventory request.
    pub fn decode_frame(frame: Frame) -> Result<Self, LegacyGossipError> {
        if frame.flags != 0 {
            return Err(LegacyGossipError::UnsupportedFlags(frame.flags));
        }

        match frame.message_type {
            MSG_REQUEST_BLOCKS_BY_HASH => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let hashes = read_hash_list(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::BlocksByHash(hashes))
            }
            MSG_REQUEST_TRANSACTIONS_BY_ID => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let ids = read_tx_id_list(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::TransactionsById(ids))
            }
            MSG_REQUEST_FIND_BLOCKS => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let (known_blocks, stop) = read_block_locator(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::FindBlocks { known_blocks, stop })
            }
            MSG_REQUEST_FIND_HEADERS => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let (known_blocks, stop) = read_block_locator(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::FindHeaders { known_blocks, stop })
            }
            MSG_REQUEST_MEMPOOL_TRANSACTION_IDS => {
                if !frame.payload.is_empty() {
                    return Err(LegacyGossipError::TrailingBytes);
                }
                Ok(Self::MempoolTransactionIds)
            }
            MSG_REQUEST_PING => {
                if !frame.payload.is_empty() {
                    return Err(LegacyGossipError::TrailingBytes);
                }
                Ok(Self::Ping)
            }
            MSG_REQUEST_PUSH_TRANSACTION => {
                let mut reader = Cursor::new(frame.payload.as_slice());
                let transaction = Transaction::zcash_deserialize(&mut reader)?;
                reject_trailing(&reader)?;
                Ok(Self::PushTransaction(UnminedTx::from(transaction)))
            }
            message_type => Err(LegacyGossipError::UnknownMessageType(message_type)),
        }
    }

    fn into_service_request(self) -> Option<Request> {
        match self {
            Self::BlocksByHash(hashes) => Some(Request::BlocksByHash(hashes.into_iter().collect())),
            Self::TransactionsById(ids) => {
                Some(Request::TransactionsById(ids.into_iter().collect()))
            }
            Self::FindBlocks { known_blocks, stop } => {
                Some(Request::FindBlocks { known_blocks, stop })
            }
            Self::FindHeaders { known_blocks, stop } => {
                Some(Request::FindHeaders { known_blocks, stop })
            }
            Self::MempoolTransactionIds => Some(Request::MempoolTransactionIds),
            Self::Ping => None,
            Self::PushTransaction(transaction) => Some(Request::PushTransaction(transaction)),
        }
    }

    fn kind(&self) -> LegacyRequestKind {
        match self {
            Self::BlocksByHash(_) => LegacyRequestKind::Blocks,
            Self::TransactionsById(_) => LegacyRequestKind::Transactions,
            Self::FindBlocks { .. } => LegacyRequestKind::FindBlocks,
            Self::FindHeaders { .. } => LegacyRequestKind::FindHeaders,
            Self::MempoolTransactionIds => LegacyRequestKind::MempoolTransactionIds,
            Self::Ping => LegacyRequestKind::Ping,
            Self::PushTransaction(_) => LegacyRequestKind::PushTransaction,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum LegacyRequestKind {
    Blocks,
    Transactions,
    FindBlocks,
    FindHeaders,
    MempoolTransactionIds,
    Ping,
    PushTransaction,
}

impl LegacyRequestKind {
    fn command(self) -> &'static str {
        match self {
            LegacyRequestKind::Blocks => "BlocksByHash",
            LegacyRequestKind::Transactions => "TransactionsById",
            LegacyRequestKind::FindBlocks => "FindBlocks",
            LegacyRequestKind::FindHeaders => "FindHeaders",
            LegacyRequestKind::MempoolTransactionIds => "MempoolTransactionIds",
            LegacyRequestKind::Ping => "Ping",
            LegacyRequestKind::PushTransaction => "PushTransaction",
        }
    }

    fn message_type(self) -> u16 {
        match self {
            LegacyRequestKind::Blocks => MSG_REQUEST_BLOCKS_BY_HASH,
            LegacyRequestKind::Transactions => MSG_REQUEST_TRANSACTIONS_BY_ID,
            LegacyRequestKind::FindBlocks => MSG_REQUEST_FIND_BLOCKS,
            LegacyRequestKind::FindHeaders => MSG_REQUEST_FIND_HEADERS,
            LegacyRequestKind::MempoolTransactionIds => MSG_REQUEST_MEMPOOL_TRANSACTION_IDS,
            LegacyRequestKind::Ping => MSG_REQUEST_PING,
            LegacyRequestKind::PushTransaction => MSG_REQUEST_PUSH_TRANSACTION,
        }
    }
}

pub(super) struct LegacyResponseCodec;

impl LegacyResponseCodec {
    pub(super) fn encode_response(
        request_id: u64,
        response: Response,
        max_frame_bytes: u32,
        max_message_bytes: u32,
    ) -> Result<Vec<Frame>, LegacyGossipError> {
        let mut frames = Vec::new();
        // Bound the cumulative response so a peer that requests many available
        // blocks/transactions cannot force us to serialize and retain an
        // unbounded `Vec<Frame>` before the first byte is written. The budget is
        // shared across every frame of this response and aborts encoding early.
        let mut budget = ResponseEncodeBudget::default();
        match response {
            Response::Blocks(blocks) => {
                let mut missing = Vec::new();
                for block in blocks {
                    match block {
                        InventoryResponse::Available((block, _)) => {
                            push_chunked_response(
                                &mut frames,
                                &mut budget,
                                MSG_RESPONSE_BLOCK,
                                request_id,
                                max_frame_bytes,
                                max_message_bytes,
                                block.zcash_serialize_to_vec()?,
                            )?;
                        }
                        InventoryResponse::Missing(hash) => missing.push(hash),
                    }
                }
                if !missing.is_empty() {
                    push_response_frame(
                        &mut frames,
                        &mut budget,
                        missing_blocks_frame(request_id, missing)?,
                    )?;
                }
            }
            Response::Transactions(transactions) => {
                let mut missing = Vec::new();
                for transaction in transactions {
                    match transaction {
                        InventoryResponse::Available((transaction, _)) => {
                            push_chunked_response(
                                &mut frames,
                                &mut budget,
                                MSG_RESPONSE_TRANSACTION,
                                request_id,
                                max_frame_bytes,
                                max_message_bytes,
                                transaction.transaction.zcash_serialize_to_vec()?,
                            )?;
                        }
                        InventoryResponse::Missing(id) => missing.push(id),
                    }
                }
                if !missing.is_empty() {
                    push_response_frame(
                        &mut frames,
                        &mut budget,
                        missing_transactions_frame(request_id, missing)?,
                    )?;
                }
            }
            Response::BlockHashes(hashes) => {
                // FindBlocks should already be service-capped; overflowing the wire cap is a bug.
                push_response_frame(
                    &mut frames,
                    &mut budget,
                    block_hashes_frame(request_id, hashes)?,
                )?;
            }
            Response::BlockHeaders(headers) => {
                // FindHeaders is protocol-capped by MAX_HEADERS_PER_MESSAGE; reject overflow.
                push_response_frame(
                    &mut frames,
                    &mut budget,
                    block_headers_frame(request_id, headers)?,
                )?;
            }
            Response::TransactionIds(ids) => {
                // Mempools can exceed one legacy inv response, so advertise the first capped page.
                push_response_frame(
                    &mut frames,
                    &mut budget,
                    transaction_ids_frame(request_id, truncate_to_inventory_cap(ids)?)?,
                )?;
            }
            Response::Pong(_) => {
                push_response_frame(
                    &mut frames,
                    &mut budget,
                    id_only_frame(MSG_RESPONSE_PONG, request_id),
                )?;
            }
            Response::Nil => {
                push_response_frame(
                    &mut frames,
                    &mut budget,
                    id_only_frame(MSG_RESPONSE_NIL, request_id),
                )?;
            }
            response => return Err(LegacyGossipError::UnexpectedResponse(response.command())),
        }
        Ok(frames)
    }

    pub(super) fn decode_response(
        request_id: u64,
        request_kind: LegacyRequestKind,
        frames: Vec<Frame>,
        requested_block_hashes: Option<&HashSet<block::Hash>>,
    ) -> Result<Response, LegacyGossipError> {
        let mut blocks = Vec::new();
        let mut transactions = Vec::new();
        let mut block_hashes = Vec::new();
        let mut block_headers = Vec::new();
        let mut transaction_ids = Vec::new();
        let mut saw_pong = false;
        let mut saw_nil = false;
        let mut reassembler = ResponseReassembler::new(request_id);

        for frame in frames {
            if frame.flags != 0 {
                return Err(LegacyGossipError::UnsupportedFlags(frame.flags));
            }

            match frame.message_type {
                MSG_RESPONSE_BLOCK => {
                    if request_kind != LegacyRequestKind::Blocks {
                        return Err(LegacyGossipError::UnexpectedResponse("Blocks"));
                    }
                    if let Some(bytes) = reassembler.accept(&frame.payload)? {
                        let block = Arc::new(Block::zcash_deserialize(&mut Cursor::new(
                            bytes.as_slice(),
                        ))?);
                        // Bind the delivered block to a hash we actually requested.
                        // Without this, a peer can substitute any other valid block
                        // for the one requested, corrupting downstream hash/source
                        // accounting (the response is correlated only by request id
                        // and kind, not by hash).
                        if let Some(requested) = requested_block_hashes {
                            let hash = block.hash();
                            if !requested.contains(&hash) {
                                return Err(LegacyGossipError::UnsolicitedBlock(hash));
                            }
                        }
                        blocks.push(InventoryResponse::Available((block, None)));
                    }
                }
                MSG_RESPONSE_TRANSACTION => {
                    if request_kind != LegacyRequestKind::Transactions {
                        return Err(LegacyGossipError::UnexpectedResponse("Transactions"));
                    }
                    if let Some(bytes) = reassembler.accept(&frame.payload)? {
                        let transaction =
                            Transaction::zcash_deserialize(&mut Cursor::new(bytes.as_slice()))?;
                        transactions.push(InventoryResponse::Available((
                            UnminedTx::from(transaction),
                            None,
                        )));
                    }
                }
                MSG_RESPONSE_MISSING_BLOCKS => {
                    if request_kind != LegacyRequestKind::Blocks {
                        return Err(LegacyGossipError::UnexpectedResponse("MissingBlocks"));
                    }
                    reassembler.reject_if_active()?;
                    for hash in decode_hashes_response(request_id, frame.payload)? {
                        // A peer may only report blocks we requested as missing.
                        if let Some(requested) = requested_block_hashes {
                            if !requested.contains(&hash) {
                                return Err(LegacyGossipError::UnsolicitedBlock(hash));
                            }
                        }
                        blocks.push(InventoryResponse::Missing(hash));
                    }
                }
                MSG_RESPONSE_MISSING_TRANSACTIONS => {
                    if request_kind != LegacyRequestKind::Transactions {
                        return Err(LegacyGossipError::UnexpectedResponse("MissingTransactions"));
                    }
                    reassembler.reject_if_active()?;
                    for id in decode_tx_ids_response(request_id, frame.payload)? {
                        transactions.push(InventoryResponse::Missing(id));
                    }
                }
                MSG_RESPONSE_BLOCK_HASHES => {
                    if request_kind != LegacyRequestKind::FindBlocks {
                        return Err(LegacyGossipError::UnexpectedResponse("BlockHashes"));
                    }
                    reassembler.reject_if_active()?;
                    block_hashes.extend(decode_hashes_response(request_id, frame.payload)?);
                }
                MSG_RESPONSE_BLOCK_HEADERS => {
                    if request_kind != LegacyRequestKind::FindHeaders {
                        return Err(LegacyGossipError::UnexpectedResponse("BlockHeaders"));
                    }
                    reassembler.reject_if_active()?;
                    block_headers.extend(decode_block_headers(request_id, frame.payload)?);
                }
                MSG_RESPONSE_TRANSACTION_IDS => {
                    if request_kind != LegacyRequestKind::MempoolTransactionIds {
                        return Err(LegacyGossipError::UnexpectedResponse("TransactionIds"));
                    }
                    reassembler.reject_if_active()?;
                    transaction_ids.extend(decode_tx_ids_response(request_id, frame.payload)?);
                }
                MSG_RESPONSE_PONG => {
                    if request_kind != LegacyRequestKind::Ping {
                        return Err(LegacyGossipError::UnexpectedResponse("Pong"));
                    }
                    reassembler.reject_if_active()?;
                    decode_id_only_response(request_id, frame.payload)?;
                    saw_pong = true;
                }
                MSG_RESPONSE_NIL => {
                    // NIL is the empty-result sentinel for chain discovery and
                    // mempool queries: the inbound service answers an empty
                    // FindBlocks/FindHeaders/MempoolTransactionIds (and a queued
                    // PushTransaction) with Response::Nil, so accept it for those
                    // kinds and let the final match below turn it into that
                    // kind's empty response. Inventory fetches and Ping must not
                    // receive a bare NIL — for BlocksByHash/TransactionsById it
                    // means the peer has none of the requested items, so reject
                    // it as unexpected and let the caller fall back to another
                    // peer (rather than silently returning an empty fetch).
                    match request_kind {
                        LegacyRequestKind::FindBlocks
                        | LegacyRequestKind::FindHeaders
                        | LegacyRequestKind::MempoolTransactionIds
                        | LegacyRequestKind::PushTransaction => {}
                        LegacyRequestKind::Blocks
                        | LegacyRequestKind::Transactions
                        | LegacyRequestKind::Ping => {
                            return Err(LegacyGossipError::UnexpectedResponse("Nil"));
                        }
                    }
                    reassembler.reject_if_active()?;
                    decode_id_only_response(request_id, frame.payload)?;
                    saw_nil = true;
                }
                message_type => return Err(LegacyGossipError::UnknownMessageType(message_type)),
            }
        }

        reassembler.finish()?;

        match request_kind {
            LegacyRequestKind::Blocks => Ok(Response::Blocks(blocks)),
            LegacyRequestKind::Transactions => Ok(Response::Transactions(transactions)),
            LegacyRequestKind::FindBlocks => Ok(Response::BlockHashes(block_hashes)),
            LegacyRequestKind::FindHeaders => Ok(Response::BlockHeaders(block_headers)),
            LegacyRequestKind::MempoolTransactionIds => {
                Ok(Response::TransactionIds(transaction_ids))
            }
            LegacyRequestKind::Ping if saw_pong => Ok(Response::Pong(Duration::ZERO)),
            LegacyRequestKind::PushTransaction if saw_nil => Ok(Response::Nil),
            LegacyRequestKind::Ping | LegacyRequestKind::PushTransaction => {
                Err(LegacyGossipError::MissingResponse(request_kind.command()))
            }
        }
    }
}

fn collect_outbound_tx_ids(
    ids: impl IntoIterator<Item = UnminedTxId>,
) -> Result<Vec<UnminedTxId>, LegacyGossipError> {
    let max_tx_inv_in_message = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE)?;
    let ids: Vec<_> = ids.into_iter().take(max_tx_inv_in_message).collect();
    if ids.is_empty() {
        return Err(LegacyGossipError::EmptyTransactionAdvertisement);
    }
    Ok(ids)
}

fn truncate_to_inventory_cap<T>(
    items: impl IntoIterator<Item = T>,
) -> Result<Vec<T>, LegacyGossipError> {
    // Legacy `inv` uses the transaction inventory cap for both tx IDs and block hashes.
    let max = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE)?;
    let mut collected = Vec::new();
    let mut dropped = 0usize;

    for item in items {
        if collected.len() < max {
            collected.push(item);
        } else {
            dropped = dropped.saturating_add(1);
        }
    }

    if dropped > 0 {
        debug!(
            dropped,
            max, "legacy inventory request exceeded item cap; dropping extras"
        );
    }

    Ok(collected)
}

fn write_hash_list(out: &mut Vec<u8>, hashes: &[block::Hash]) -> Result<(), LegacyGossipError> {
    ensure_inventory_count(hashes.len())?;
    CompactSizeMessage::try_from(hashes.len())?.zcash_serialize(&mut *out)?;
    for hash in hashes {
        hash.zcash_serialize(&mut *out)?;
    }
    Ok(())
}

fn read_hash_list(reader: &mut Cursor<&[u8]>) -> Result<Vec<block::Hash>, LegacyGossipError> {
    let count = bounded_inventory_count(reader)?;
    let mut hashes = Vec::with_capacity(count);
    for _ in 0..count {
        hashes.push(block::Hash::zcash_deserialize(&mut *reader)?);
    }
    Ok(hashes)
}

fn write_block_locator(
    out: &mut Vec<u8>,
    known_blocks: &[block::Hash],
    stop: Option<block::Hash>,
) -> Result<(), LegacyGossipError> {
    ensure_block_locator_count(known_blocks.len())?;
    CompactSizeMessage::try_from(known_blocks.len())?.zcash_serialize(&mut *out)?;
    for hash in known_blocks {
        hash.zcash_serialize(&mut *out)?;
    }
    stop.unwrap_or(NO_STOP_HASH).zcash_serialize(&mut *out)?;
    Ok(())
}

fn read_block_locator(
    reader: &mut Cursor<&[u8]>,
) -> Result<(Vec<block::Hash>, Option<block::Hash>), LegacyGossipError> {
    let count = bounded_block_locator_count(reader)?;
    let mut known_blocks = Vec::with_capacity(count);
    for _ in 0..count {
        known_blocks.push(block::Hash::zcash_deserialize(&mut *reader)?);
    }
    let stop_hash = block::Hash::zcash_deserialize(&mut *reader)?;
    // A zero stop hash is the legacy locator sentinel for "no stop hash".
    let stop = (stop_hash != NO_STOP_HASH).then_some(stop_hash);
    Ok((known_blocks, stop))
}

fn write_tx_id_list(out: &mut Vec<u8>, ids: &[UnminedTxId]) -> Result<(), LegacyGossipError> {
    ensure_inventory_count(ids.len())?;
    CompactSizeMessage::try_from(ids.len())?.zcash_serialize(&mut *out)?;
    for id in ids {
        InventoryHash::from(id).zcash_serialize(&mut *out)?;
    }
    Ok(())
}

fn read_tx_id_list(reader: &mut Cursor<&[u8]>) -> Result<Vec<UnminedTxId>, LegacyGossipError> {
    let count = bounded_inventory_count(reader)?;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let inv = InventoryHash::zcash_deserialize(&mut *reader)?;
        let Some(id) = inv.unmined_tx_id() else {
            return Err(LegacyGossipError::NonTransactionInventory);
        };
        ids.push(id);
    }
    Ok(ids)
}

fn bounded_inventory_count(reader: &mut Cursor<&[u8]>) -> Result<usize, LegacyGossipError> {
    let count = usize::from(CompactSizeMessage::zcash_deserialize(reader)?);
    ensure_inventory_count(count)?;
    Ok(count)
}

fn ensure_inventory_count(count: usize) -> Result<(), LegacyGossipError> {
    let max = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE)?;
    if count > max {
        return Err(LegacyGossipError::TooManyInventoryItems(count));
    }
    Ok(())
}

fn bounded_block_locator_count(reader: &mut Cursor<&[u8]>) -> Result<usize, LegacyGossipError> {
    let count = usize::from(CompactSizeMessage::zcash_deserialize(reader)?);
    ensure_block_locator_count(count)?;
    Ok(count)
}

fn ensure_block_locator_count(count: usize) -> Result<(), LegacyGossipError> {
    let max = usize::try_from(MAX_BLOCK_LOCATOR_LENGTH)?;
    if count > max {
        return Err(LegacyGossipError::TooManyBlockLocatorHashes(count));
    }
    Ok(())
}

fn write_header_list(
    out: &mut Vec<u8>,
    headers: &[block::CountedHeader],
) -> Result<(), LegacyGossipError> {
    ensure_header_count(headers.len())?;
    CompactSizeMessage::try_from(headers.len())?.zcash_serialize(&mut *out)?;
    for header in headers {
        header.zcash_serialize(&mut *out)?;
    }
    Ok(())
}

fn read_header_list(
    reader: &mut Cursor<&[u8]>,
) -> Result<Vec<block::CountedHeader>, LegacyGossipError> {
    let count = bounded_header_count(reader)?;
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        headers.push(block::CountedHeader::zcash_deserialize(&mut *reader)?);
    }
    Ok(headers)
}

fn bounded_header_count(reader: &mut Cursor<&[u8]>) -> Result<usize, LegacyGossipError> {
    let count = usize::from(CompactSizeMessage::zcash_deserialize(reader)?);
    ensure_header_count(count)?;
    Ok(count)
}

fn ensure_header_count(count: usize) -> Result<(), LegacyGossipError> {
    if count > MAX_HEADERS_PER_MESSAGE {
        return Err(LegacyGossipError::TooManyHeaders(count));
    }
    Ok(())
}

/// Tracks the cumulative size of an encoded legacy response so the inbound
/// responder aborts a single request's response early instead of buffering an
/// unbounded `Vec<Frame>` before the first byte is written. The outbound reader
/// enforces a symmetric per-response cap via `LegacyResponseBudget`.
#[derive(Default)]
struct ResponseEncodeBudget {
    bytes: usize,
}

impl ResponseEncodeBudget {
    /// Account for one buffered response frame's payload, rejecting the whole
    /// response once cumulative payload bytes exceed the responder aggregate
    /// budget.
    fn account(&mut self, payload_len: usize) -> Result<(), LegacyGossipError> {
        self.bytes = self.bytes.saturating_add(payload_len);
        if self.bytes > LEGACY_RESPONSE_MAX_AGGREGATE_BYTES {
            return Err(LegacyGossipError::ResponseAggregateBudget(self.bytes));
        }
        Ok(())
    }
}

/// Push one fully-built response frame, charging it against the aggregate
/// budget first so an over-budget response aborts before it is retained.
fn push_response_frame(
    frames: &mut Vec<Frame>,
    budget: &mut ResponseEncodeBudget,
    frame: Frame,
) -> Result<(), LegacyGossipError> {
    budget.account(frame.payload.len())?;
    frames.push(frame);
    Ok(())
}

fn push_chunked_response(
    frames: &mut Vec<Frame>,
    budget: &mut ResponseEncodeBudget,
    message_type: u16,
    request_id: u64,
    max_frame_bytes: u32,
    max_message_bytes: u32,
    bytes: Vec<u8>,
) -> Result<(), LegacyGossipError> {
    if bytes.len() > MAX_PROTOCOL_MESSAGE_LEN {
        return Err(LegacyGossipError::OversizedResponse(bytes.len()));
    }

    // Size each chunk frame against the *effective* outbound cap: the smaller of
    // the negotiated frame payload cap (`max_frame_bytes - FRAME_HEADER_BYTES`)
    // and the peer's negotiated `max_message_bytes`. The handshake clamps the two
    // caps independently, so sizing against the frame cap alone produces chunk
    // frames whose payload exceeds the peer's accepted message cap, which the
    // peer (and our own `write_response_frame`) reject as oversize.
    let max_payload_bytes = usize::try_from(max_frame_bytes)?
        .saturating_sub(FRAME_HEADER_BYTES)
        .min(usize::try_from(max_message_bytes)?);
    let max_chunk_bytes = max_payload_bytes
        .checked_sub(RESPONSE_CHUNK_HEADER_BYTES)
        .ok_or(LegacyGossipError::OversizedResponse(bytes.len()))?;
    if max_chunk_bytes == 0 {
        return Err(LegacyGossipError::OversizedResponse(bytes.len()));
    }
    let chunk_bytes = LEGACY_RESPONSE_CHUNK_BYTES.min(max_chunk_bytes);

    debug_assert!(!bytes.is_empty());
    let chunks = bytes.chunks(chunk_bytes).collect::<Vec<_>>();

    for (index, chunk) in chunks.iter().enumerate() {
        let mut payload = Vec::with_capacity(RESPONSE_CHUNK_HEADER_BYTES + chunk.len());
        // The stream prelude already has this id, but each response frame repeats it so
        // malformed peers are rejected at the frame boundary before decoding payload bytes.
        payload.extend_from_slice(&request_id.to_le_bytes());
        payload.push(u8::from(index + 1 == chunks.len()));
        payload.extend_from_slice(chunk);
        // Charge each chunk against the shared budget so a response that
        // aggregates many available items aborts mid-encode rather than after
        // the whole `Vec<Frame>` has been materialized.
        budget.account(payload.len())?;
        frames.push(Frame {
            message_type,
            flags: 0,
            payload,
        });
    }
    Ok(())
}

struct ResponseReassembler {
    expected_id: u64,
    active: bool,
    buffer: Vec<u8>,
}

impl ResponseReassembler {
    fn new(expected_id: u64) -> Self {
        Self {
            expected_id,
            active: false,
            buffer: Vec::new(),
        }
    }

    fn accept(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>, LegacyGossipError> {
        let (response_id, is_last, bytes) = decode_response_chunk_header(payload)?;
        if response_id != self.expected_id {
            return Err(LegacyGossipError::WrongRequestId {
                expected: self.expected_id,
                actual: response_id,
            });
        }

        let new_len = self
            .buffer
            .len()
            .checked_add(bytes.len())
            .ok_or(LegacyGossipError::OversizedResponse(usize::MAX))?;
        if new_len > MAX_PROTOCOL_MESSAGE_LEN {
            return Err(LegacyGossipError::OversizedResponse(new_len));
        }
        self.active = true;
        self.buffer.extend_from_slice(bytes);

        if is_last {
            self.active = false;
            Ok(Some(std::mem::take(&mut self.buffer)))
        } else {
            Ok(None)
        }
    }

    fn reject_if_active(&self) -> Result<(), LegacyGossipError> {
        if self.active {
            Err(LegacyGossipError::IncompleteResponseChunk)
        } else {
            Ok(())
        }
    }

    fn finish(self) -> Result<(), LegacyGossipError> {
        if self.active {
            Err(LegacyGossipError::IncompleteResponseChunk)
        } else {
            Ok(())
        }
    }
}

fn decode_response_chunk_header(payload: &[u8]) -> Result<(u64, bool, &[u8]), LegacyGossipError> {
    if payload.len() < RESPONSE_CHUNK_HEADER_BYTES {
        return Err(LegacyGossipError::TruncatedResponse);
    }
    let (request_id, payload) = read_request_id(payload)?;
    Ok((request_id, payload[0] != 0, &payload[1..]))
}

fn missing_blocks_frame(
    request_id: u64,
    hashes: Vec<block::Hash>,
) -> Result<Frame, LegacyGossipError> {
    let mut payload = Vec::new();
    // Missing responses also repeat the id, keeping all response-frame validation local.
    payload.extend_from_slice(&request_id.to_le_bytes());
    write_hash_list(&mut payload, &hashes)?;
    Ok(Frame {
        message_type: MSG_RESPONSE_MISSING_BLOCKS,
        flags: 0,
        payload,
    })
}

fn missing_transactions_frame(
    request_id: u64,
    ids: Vec<UnminedTxId>,
) -> Result<Frame, LegacyGossipError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&request_id.to_le_bytes());
    write_tx_id_list(&mut payload, &ids)?;
    Ok(Frame {
        message_type: MSG_RESPONSE_MISSING_TRANSACTIONS,
        flags: 0,
        payload,
    })
}

fn block_hashes_frame(
    request_id: u64,
    hashes: Vec<block::Hash>,
) -> Result<Frame, LegacyGossipError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&request_id.to_le_bytes());
    write_hash_list(&mut payload, &hashes)?;
    Ok(Frame {
        message_type: MSG_RESPONSE_BLOCK_HASHES,
        flags: 0,
        payload,
    })
}

fn block_headers_frame(
    request_id: u64,
    headers: Vec<block::CountedHeader>,
) -> Result<Frame, LegacyGossipError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&request_id.to_le_bytes());
    write_header_list(&mut payload, &headers)?;
    Ok(Frame {
        message_type: MSG_RESPONSE_BLOCK_HEADERS,
        flags: 0,
        payload,
    })
}

fn transaction_ids_frame(
    request_id: u64,
    ids: Vec<UnminedTxId>,
) -> Result<Frame, LegacyGossipError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&request_id.to_le_bytes());
    write_tx_id_list(&mut payload, &ids)?;
    Ok(Frame {
        message_type: MSG_RESPONSE_TRANSACTION_IDS,
        flags: 0,
        payload,
    })
}

fn id_only_frame(message_type: u16, request_id: u64) -> Frame {
    Frame {
        message_type,
        flags: 0,
        payload: request_id.to_le_bytes().to_vec(),
    }
}

fn read_request_id(payload: &[u8]) -> Result<(u64, &[u8]), LegacyGossipError> {
    if payload.len() < REQUEST_ID_BYTES {
        return Err(LegacyGossipError::TruncatedResponse);
    }
    let mut id_bytes = [0; REQUEST_ID_BYTES];
    id_bytes.copy_from_slice(&payload[..REQUEST_ID_BYTES]);
    Ok((u64::from_le_bytes(id_bytes), &payload[REQUEST_ID_BYTES..]))
}

fn verify_response_id(request_id: u64, payload: &[u8]) -> Result<&[u8], LegacyGossipError> {
    let (response_id, payload) = read_request_id(payload)?;
    if response_id != request_id {
        return Err(LegacyGossipError::WrongRequestId {
            expected: request_id,
            actual: response_id,
        });
    }
    Ok(payload)
}

fn decode_hashes_response(
    request_id: u64,
    payload: Vec<u8>,
) -> Result<Vec<block::Hash>, LegacyGossipError> {
    let payload = verify_response_id(request_id, &payload)?;
    let mut reader = Cursor::new(payload);
    let hashes = read_hash_list(&mut reader)?;
    reject_trailing(&reader)?;
    Ok(hashes)
}

fn decode_tx_ids_response(
    request_id: u64,
    payload: Vec<u8>,
) -> Result<Vec<UnminedTxId>, LegacyGossipError> {
    let payload = verify_response_id(request_id, &payload)?;
    let mut reader = Cursor::new(payload);
    let ids = read_tx_id_list(&mut reader)?;
    reject_trailing(&reader)?;
    Ok(ids)
}

fn decode_block_headers(
    request_id: u64,
    payload: Vec<u8>,
) -> Result<Vec<block::CountedHeader>, LegacyGossipError> {
    let payload = verify_response_id(request_id, &payload)?;
    let mut reader = Cursor::new(payload);
    let headers = read_header_list(&mut reader)?;
    reject_trailing(&reader)?;
    Ok(headers)
}

fn decode_id_only_response(request_id: u64, payload: Vec<u8>) -> Result<(), LegacyGossipError> {
    let payload = verify_response_id(request_id, &payload)?;
    if !payload.is_empty() {
        return Err(LegacyGossipError::TrailingBytes);
    }
    Ok(())
}

fn outbound_tx_id_slice(ids: &[UnminedTxId]) -> Result<&[UnminedTxId], LegacyGossipError> {
    let max_tx_inv_in_message = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE)?;
    let ids = &ids[..ids.len().min(max_tx_inv_in_message)];
    if ids.is_empty() {
        return Err(LegacyGossipError::EmptyTransactionAdvertisement);
    }
    Ok(ids)
}

fn reject_trailing(reader: &Cursor<&[u8]>) -> Result<(), LegacyGossipError> {
    let len = u64::try_from(reader.get_ref().len())?;
    if reader.position() != len {
        return Err(LegacyGossipError::TrailingBytes);
    }
    Ok(())
}

/// Broadcasts legacy gossip frames to outbound-ready Zakura peers.
#[derive(Clone, Debug)]
pub struct ZakuraGossipBroadcast {
    first_seen: FirstSeenCache,
    outbound: LegacyGossipOutbound,
}

impl ZakuraGossipBroadcast {
    /// Create a broadcaster from a Zakura supervisor.
    pub fn new(supervisor: ZakuraSupervisorHandle) -> Self {
        let first_seen = first_seen_for_supervisor(&supervisor);
        let outbound = outbound_for_supervisor(&supervisor);
        Self {
            first_seen,
            outbound,
        }
    }

    async fn broadcast(
        &self,
        frame: LegacyGossipFrame,
        exclude: Option<&ZakuraPeerId>,
    ) -> Result<(), BoxError> {
        self.outbound.remember_latest_block(&frame);
        self.outbound.send_to_peers(frame, exclude).await
    }

    async fn record_first_seen(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        self.first_seen.record_first_seen(frame).await
    }

    async fn unseen(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        self.first_seen.unseen(frame).await
    }

    async fn mark_seen(&self, frame: &LegacyGossipFrame) {
        self.first_seen.record_seen(frame).await;
    }
}

#[derive(Clone, Debug, Default)]
struct LegacyGossipOutbound {
    sessions: Arc<StdMutex<HashMap<ZakuraPeerId, LegacyGossipPeerSession>>>,
    latest_block: Arc<StdMutex<Option<block::Hash>>>,
}

impl LegacyGossipOutbound {
    fn insert(&self, session: LegacyGossipPeerSession) {
        self.sessions
            .lock()
            .expect("legacy gossip outbound mutex is never poisoned")
            .insert(session.peer_id().clone(), session);
    }

    fn remove(&self, peer: &ZakuraPeerId) {
        self.sessions
            .lock()
            .expect("legacy gossip outbound mutex is never poisoned")
            .remove(peer);
    }

    #[cfg(test)]
    fn contains(&self, peer: &ZakuraPeerId) -> bool {
        self.sessions
            .lock()
            .expect("legacy gossip outbound mutex is never poisoned")
            .contains_key(peer)
    }

    fn remember_latest_block(&self, frame: &LegacyGossipFrame) {
        if let LegacyGossipFrame::AdvertiseBlock(hash) = frame {
            *self
                .latest_block
                .lock()
                .expect("legacy gossip latest-block mutex is never poisoned") = Some(*hash);
        }
    }

    async fn replay_latest_block_to_peer(
        &self,
        session: LegacyGossipPeerSession,
    ) -> Result<(), BoxError> {
        let Some(hash) = *self
            .latest_block
            .lock()
            .expect("legacy gossip latest-block mutex is never poisoned")
        else {
            return Ok(());
        };

        session.try_send_advertise_block(hash).map_err(Into::into)
    }

    async fn send_to_peers(
        &self,
        frame: LegacyGossipFrame,
        exclude: Option<&ZakuraPeerId>,
    ) -> Result<(), BoxError> {
        let sessions: Vec<_> = {
            let sessions = self
                .sessions
                .lock()
                .expect("legacy gossip outbound mutex is never poisoned");
            sessions
                .iter()
                .filter(|(peer_id, _)| !exclude.is_some_and(|exclude| exclude == *peer_id))
                .map(|(_peer_id, session)| session.clone())
                .collect()
        };

        send_to_sessions(sessions, frame)
    }
}

#[cfg(test)]
fn legacy_gossip_recv_loop_panic_target() -> &'static StdMutex<Option<ZakuraPeerId>> {
    static TARGET: OnceLock<StdMutex<Option<ZakuraPeerId>>> = OnceLock::new();
    TARGET.get_or_init(Default::default)
}

#[cfg(test)]
fn arm_legacy_gossip_recv_loop_panic(peer: ZakuraPeerId) {
    *legacy_gossip_recv_loop_panic_target()
        .lock()
        .expect("legacy gossip recv-loop panic target mutex is never poisoned") = Some(peer);
}

#[cfg(test)]
fn should_panic_legacy_gossip_recv_loop(peer: &ZakuraPeerId) -> bool {
    let mut target = legacy_gossip_recv_loop_panic_target()
        .lock()
        .expect("legacy gossip recv-loop panic target mutex is never poisoned");
    if target.as_ref() == Some(peer) {
        *target = None;
        true
    } else {
        false
    }
}

/// Typed ordered legacy gossip sender for one peer.
#[derive(Clone, Debug)]
pub struct LegacyGossipPeerSession {
    peer_id: ZakuraPeerId,
    send: FramedSend,
}

impl LegacyGossipPeerSession {
    fn new(peer_id: ZakuraPeerId, send: FramedSend) -> Self {
        Self { peer_id, send }
    }

    /// Authenticated peer identity for this legacy gossip stream.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    /// Send an explicit block advertisement.
    pub fn try_send_advertise_block(&self, hash: block::Hash) -> Result<(), OrderedSendError> {
        self.try_send_gossip_frame(LegacyGossipFrame::AdvertiseBlock(hash))
    }

    /// Send explicit transaction id advertisements.
    pub fn try_send_advertise_transaction_ids(
        &self,
        ids: Vec<UnminedTxId>,
    ) -> Result<(), OrderedSendError> {
        self.try_send_gossip_frame(LegacyGossipFrame::AdvertiseTransactionIds(ids))
    }

    fn try_send_gossip_frame(&self, frame: LegacyGossipFrame) -> Result<(), OrderedSendError> {
        let frame = frame
            .encode_frame()
            .map_err(|error| OrderedSendError::Encode(Box::new(error)))?;
        match self.send.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_frame)) => Err(OrderedSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_frame)) => Err(OrderedSendError::Closed),
        }
    }
}

static LEGACY_GOSSIP_OUTBOUND_BY_SUPERVISOR: OnceLock<
    std::sync::Mutex<HashMap<u64, LegacyGossipOutbound>>,
> = OnceLock::new();

fn outbound_for_supervisor(supervisor: &ZakuraSupervisorHandle) -> LegacyGossipOutbound {
    let registry = LEGACY_GOSSIP_OUTBOUND_BY_SUPERVISOR.get_or_init(Default::default);
    let mut registry = registry
        .lock()
        .expect("legacy gossip outbound registry mutex is never poisoned");
    registry.entry(supervisor.id()).or_default().clone()
}

fn first_seen_for_supervisor(supervisor: &ZakuraSupervisorHandle) -> FirstSeenCache {
    let registry = FIRST_SEEN_BY_SUPERVISOR.get_or_init(Default::default);
    let mut registry = registry
        .lock()
        .expect("legacy gossip first-seen registry mutex is never poisoned");
    registry
        .entry(supervisor.id())
        .or_insert_with(|| FirstSeenCache::new(DEFAULT_FIRST_SEEN_CAPACITY, DEFAULT_FIRST_SEEN_TTL))
        .clone()
}

fn send_to_sessions(
    sessions: Vec<LegacyGossipPeerSession>,
    frame: LegacyGossipFrame,
) -> Result<(), BoxError> {
    let mut first_error = None;
    for session in sessions {
        let result = match &frame {
            LegacyGossipFrame::AdvertiseBlock(hash) => session.try_send_advertise_block(*hash),
            LegacyGossipFrame::AdvertiseTransactionIds(ids) => {
                session.try_send_advertise_transaction_ids(ids.clone())
            }
        };

        if let Err(error) = result {
            debug!(
                peer = ?session.peer_id(),
                ?error,
                "failed to queue Zakura legacy gossip frame"
            );
            if first_error.is_none() {
                first_error = Some(error.into());
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

/// Tower service that adapts legacy Zebra gossip requests onto Zakura streams.
#[derive(Clone, Debug)]
pub struct LegacyGossipAdapter {
    broadcast: ZakuraGossipBroadcast,
}

impl LegacyGossipAdapter {
    /// Create an adapter backed by the given Zakura supervisor.
    pub fn new(supervisor: ZakuraSupervisorHandle) -> Self {
        Self {
            broadcast: ZakuraGossipBroadcast::new(supervisor),
        }
    }
}

impl Service<Request> for LegacyGossipAdapter {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let broadcast = self.broadcast.clone();
        Box::pin(async move {
            let frame = LegacyGossipFrame::from_request(request)?;
            let Some(frame) = broadcast.record_first_seen(&frame).await else {
                return Ok(Response::Nil);
            };
            broadcast.broadcast(frame, None).await?;
            Ok(Response::Nil)
        })
    }
}

/// Tower service that adapts legacy request/response traffic onto Zakura streams.
///
/// Legacy `Peers` discovery is intentionally not adapted here. Zakura v1 controlled
/// networks use configured [`crate::zakura::ZakuraConfig::bootstrap_peers`] until
/// native Zakura discovery is available, keeping this compatibility layer narrow.
#[derive(Clone, Debug)]
pub struct LegacyRequestAdapter {
    client: ZakuraRequestClient,
}

impl LegacyRequestAdapter {
    /// Create an adapter backed by the given Zakura supervisor.
    pub fn new(supervisor: ZakuraSupervisorHandle) -> Self {
        Self {
            client: ZakuraRequestClient::new(supervisor),
        }
    }

    /// Create an adapter backed by the given Zakura supervisor and trace emitter.
    pub fn new_with_trace(supervisor: ZakuraSupervisorHandle, trace: ZakuraTrace) -> Self {
        Self {
            client: ZakuraRequestClient::new_with_trace(supervisor, trace),
        }
    }

    #[cfg(test)]
    fn new_with_timeout(supervisor: ZakuraSupervisorHandle, request_timeout: Duration) -> Self {
        Self {
            client: ZakuraRequestClient::new_with_timeout(supervisor, request_timeout),
        }
    }

    /// Request inventory, preferring the Zakura peer that advertised it when known.
    pub async fn request_from_source(
        &self,
        request: Request,
        source: Option<PeerSource>,
    ) -> Result<Response, BoxError> {
        let frame = LegacyRequestFrame::from_request(request)?;
        self.client.request(frame, source, false).await
    }
}

impl Service<Request> for LegacyRequestAdapter {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let client = self.client.clone();
        Box::pin(async move {
            let source = request.inventory_source();
            let retry_source_missing = source.is_some();
            let frame = LegacyRequestFrame::from_request(request)?;
            client.request(frame, source, retry_source_missing).await
        })
    }
}

/// Which transports a [`ZakuraDualStackService`] request fans out to.
#[derive(Copy, Clone)]
enum DualStackRoute {
    /// Block/transaction advertisements: fan out to both stacks, fire-and-forget.
    Advertise,
    /// Requests the Zakura adapter can serve (inventory fetches, chain-sync
    /// discovery, and mempool data): legacy peer set first, then Zakura fallback.
    ///
    /// This must cover every request a node needs to follow the chain, otherwise
    /// a node whose only peer was upgraded to Zakura sends them to its empty
    /// legacy peer set and stalls (its syncer can never obtain tips or fetch
    /// blocks). The set mirrors `LegacyRequestFrame::from_request`.
    LegacyFirstThenZakura,
    /// Anything the Zakura adapter cannot serve (e.g. peer-address discovery):
    /// legacy peer set only.
    Passthrough,
}

/// Tower service that fans legacy gossip/inventory requests across both the
/// legacy TCP peer set and the Zakura P2P-v2 transport.
///
/// This is the production seam that turns the Zakura legacy-gossip adapters from
/// an isolated library into a live dual-stack: it wraps the legacy `peer_set`
/// returned by [`crate::init`] when `v2_p2p` is enabled, so the syncer, mempool,
/// and inbound downloads transparently gossip and fetch over Zakura too.
///
/// Routing (with `legacy_enabled` = `config.legacy_p2p`):
/// - Advertisements fan out concurrently to the legacy peer set (when
///   `legacy_enabled`) and the Zakura gossip adapter. Advertise is
///   fire-and-forget, so per-path errors are logged and swallowed and the
///   composite always reports success — one stack failing never fails the local
///   advertisement.
/// - Inventory fetches hit the legacy peer set first and fall back to Zakura
///   when the legacy response is all-missing or errors. With `legacy_enabled`
///   false they route straight to Zakura.
/// - Every other request passes through to the legacy peer set.
///
/// The gossip and request adapters MUST be built from the same
/// [`ZakuraSupervisorHandle`] that backs the inbound [`LegacyGossipSink`], so the
/// per-supervisor first-seen cache (see [`first_seen_for_supervisor`]) dedups
/// locally originated gossip against echoes that peers send back to us.
#[derive(Clone)]
pub(crate) struct ZakuraDualStackService<L> {
    legacy: L,
    gossip: LegacyGossipAdapter,
    request: LegacyRequestAdapter,
    legacy_enabled: bool,
}

impl<L> ZakuraDualStackService<L> {
    /// Wrap `legacy` with Zakura gossip/request adapters backed by `supervisor`.
    ///
    /// `supervisor` must be the endpoint's supervisor so the first-seen cache is
    /// shared with the inbound sink.
    #[cfg(test)]
    pub(crate) fn new(legacy: L, supervisor: ZakuraSupervisorHandle, legacy_enabled: bool) -> Self {
        Self {
            legacy,
            gossip: LegacyGossipAdapter::new(supervisor.clone()),
            request: LegacyRequestAdapter::new(supervisor),
            legacy_enabled,
        }
    }

    /// Wrap `legacy` with Zakura adapters and a trace emitter.
    pub(crate) fn new_with_trace(
        legacy: L,
        supervisor: ZakuraSupervisorHandle,
        legacy_enabled: bool,
        trace: ZakuraTrace,
    ) -> Self {
        Self {
            legacy,
            gossip: LegacyGossipAdapter::new(supervisor.clone()),
            request: LegacyRequestAdapter::new_with_trace(supervisor, trace),
            legacy_enabled,
        }
    }
}

impl<L> Service<Request> for ZakuraDualStackService<L>
where
    L: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    L::Future: Send + 'static,
{
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The route is request-dependent, so legacy readiness is awaited inside
        // the branches that actually use legacy. Source-aware Zakura requests
        // must not block behind an empty or unready legacy peer set.
        std::task::ready!(self.gossip.poll_ready(cx))?;
        std::task::ready!(self.request.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let route = match &request {
            Request::AdvertiseBlock(..)
            | Request::AdvertiseBlockToAll(..)
            | Request::AdvertiseTransactionIds(..) => DualStackRoute::Advertise,
            Request::BlocksByHash(..)
            | Request::BlocksByHashFrom { .. }
            | Request::TransactionsById(..)
            | Request::TransactionsByIdFrom { .. }
            | Request::FindBlocks { .. }
            | Request::FindHeaders { .. }
            | Request::MempoolTransactionIds
            | Request::PushTransaction(..) => DualStackRoute::LegacyFirstThenZakura,
            _ => DualStackRoute::Passthrough,
        };

        let legacy_enabled = self.legacy_enabled;
        let mut gossip = self.gossip.clone();
        let mut request_adapter = self.request.clone();
        let mut legacy = self.legacy.clone();

        Box::pin(async move {
            match route {
                DualStackRoute::Advertise => {
                    // Fan out concurrently so a slow/empty Zakura path can't delay
                    // legacy advertise, and vice versa. The gossip adapter bounds
                    // each peer send with its own per-peer fanout timeout.
                    let gossip_fut = gossip.call(request.clone());
                    let legacy_fut = async move {
                        if legacy_enabled {
                            Some(match legacy.ready().await {
                                Ok(service) => service.call(request).await,
                                Err(error) => Err(error),
                            })
                        } else {
                            None
                        }
                    };
                    let (legacy_res, gossip_res) = futures::join!(legacy_fut, gossip_fut);
                    if let Some(Err(error)) = legacy_res {
                        debug!(%error, "legacy advertise path failed; continuing");
                    }
                    if let Err(error) = gossip_res {
                        debug!(%error, "zakura advertise path failed; continuing");
                    }
                    Ok(Response::Nil)
                }
                DualStackRoute::LegacyFirstThenZakura => {
                    if matches!(request.inventory_source(), Some(PeerSource::Zakura(_))) {
                        return request_adapter.call(request).await;
                    }
                    if !legacy_enabled {
                        return request_adapter.call(request).await;
                    }
                    // The legacy peer set is buffered, so it queues this fetch
                    // even when it has no ready peer (e.g. every legacy peer was
                    // upgraded to Zakura). Bound the legacy attempt so we fall
                    // back to the Zakura path instead of blocking forever.
                    let legacy_attempt = timeout(DUAL_STACK_LEGACY_INVENTORY_TIMEOUT, async {
                        legacy.ready().await?.call(request.clone()).await
                    })
                    .await;
                    match legacy_attempt {
                        Ok(Ok(response)) if !all_inventory_missing(&response) => Ok(response),
                        Ok(Ok(response)) => match request_adapter.call(request).await {
                            Ok(zakura) if !all_inventory_missing(&zakura) => Ok(zakura),
                            _ => Ok(response),
                        },
                        Ok(Err(legacy_error)) => match request_adapter.call(request).await {
                            Ok(zakura) => Ok(zakura),
                            Err(_) => Err(legacy_error),
                        },
                        // Legacy timed out (no ready peer): use Zakura.
                        Err(_) => request_adapter.call(request).await,
                    }
                }
                DualStackRoute::Passthrough => legacy.ready().await?.call(request).await,
            }
        })
    }
}

/// Outbound Zakura inventory request client.
#[derive(Clone, Debug)]
pub struct ZakuraRequestClient {
    supervisor: ZakuraSupervisorHandle,
    request_timeout: Duration,
    trace: ZakuraTrace,
}

impl ZakuraRequestClient {
    /// Create a client from a Zakura supervisor.
    pub fn new(supervisor: ZakuraSupervisorHandle) -> Self {
        Self::new_with_trace(supervisor, ZakuraTrace::noop())
    }

    /// Create a client from a Zakura supervisor and trace emitter.
    pub fn new_with_trace(supervisor: ZakuraSupervisorHandle, trace: ZakuraTrace) -> Self {
        Self {
            supervisor,
            request_timeout: LEGACY_REQUEST_TIMEOUT,
            trace,
        }
    }

    #[cfg(test)]
    fn new_with_timeout(supervisor: ZakuraSupervisorHandle, request_timeout: Duration) -> Self {
        Self {
            supervisor,
            request_timeout,
            trace: ZakuraTrace::noop(),
        }
    }

    async fn request(
        &self,
        frame: LegacyRequestFrame,
        source: Option<PeerSource>,
        retry_source_missing: bool,
    ) -> Result<Response, BoxError> {
        let preferred = match source {
            Some(PeerSource::Zakura(peer_id)) => Some(peer_id),
            _ => None,
        };

        let handles = self.ready_handles().await?;
        let Some(primary) = select_handle(&handles, preferred.as_ref()) else {
            return Err("no ready Zakura peer for legacy inventory request".into());
        };

        let request_kind = frame.kind();
        let first_result = self
            .request_one(primary.clone(), frame.clone(), request_kind)
            .await;
        match first_result {
            Ok(response) if !all_inventory_missing(&response) => Ok(response),
            Ok(response) => {
                let Some(fallback) = select_fallback_handle(&handles, primary.peer_id()) else {
                    if retry_source_missing && preferred.is_some() {
                        return self
                            .retry_source_missing(primary, frame, request_kind, response)
                            .await;
                    }
                    return Ok(response);
                };
                self.request_one(fallback, frame, request_kind)
                    .await
                    .or(Ok(response))
            }
            Err(error) => {
                let Some(fallback) = select_fallback_handle(&handles, primary.peer_id()) else {
                    return Err(error);
                };
                self.request_one(fallback, frame, request_kind).await
            }
        }
    }

    async fn retry_source_missing(
        &self,
        handle: ZakuraPeerHandle,
        frame: LegacyRequestFrame,
        request_kind: LegacyRequestKind,
        initial_response: Response,
    ) -> Result<Response, BoxError> {
        let mut last_response = initial_response;
        for _ in 0..SOURCE_INVENTORY_MISSING_RETRIES {
            sleep(SOURCE_INVENTORY_MISSING_RETRY_DELAY).await;
            match self
                .request_one(handle.clone(), frame.clone(), request_kind)
                .await
            {
                Ok(response) if !all_inventory_missing(&response) => return Ok(response),
                Ok(response) => last_response = response,
                Err(error) => return Err(error),
            }
        }
        Ok(last_response)
    }

    async fn ready_handles(&self) -> Result<Vec<ZakuraPeerHandle>, BoxError> {
        let mut registered = self.supervisor.subscribe();

        timeout(LEGACY_REQUEST_READY_TIMEOUT, async {
            loop {
                let handles = self.supervisor.outbound_peer_handles().await;
                if !handles.is_empty() {
                    return Ok(handles);
                }

                if registered.changed().await.is_err() {
                    return Err("Zakura peer set closed before a peer was ready".into());
                }
            }
        })
        .await
        .map_err(|_| -> BoxError { "no ready Zakura peer for legacy inventory request".into() })?
    }

    async fn request_one(
        &self,
        handle: ZakuraPeerHandle,
        frame: LegacyRequestFrame,
        request_kind: LegacyRequestKind,
    ) -> Result<Response, BoxError> {
        let request_id = NEXT_LEGACY_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        // Capture the requested block hashes (if any) before consuming the frame,
        // so the response can be bound to a hash we actually asked for.
        let requested_block_hashes: Option<HashSet<block::Hash>> = match &frame {
            LegacyRequestFrame::BlocksByHash(hashes) => Some(hashes.iter().copied().collect()),
            _ => None,
        };
        let frame = frame.encode_frame()?;
        trace_legacy_request_start(
            &self.trace,
            "outbound.request",
            Some(handle.peer_id()),
            request_id,
            request_kind,
            frame.message_type,
        );
        let started_at = Instant::now();
        let response = match timeout(
            self.request_timeout,
            handle.request(
                ZAKURA_STREAM_LEGACY_REQUESTS,
                request_id,
                frame.message_type,
                frame.flags,
                frame.payload,
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                trace_legacy_request_error(
                    &self.trace,
                    "outbound.error",
                    Some(handle.peer_id()),
                    request_id,
                    request_kind.command(),
                    error.to_string(),
                );
                return Err(error);
            }
            Err(_) => {
                let error: BoxError = format!(
                    "Zakura legacy request timed out for peer {:?}",
                    handle.peer_id()
                )
                .into();
                trace_legacy_request_error(
                    &self.trace,
                    "outbound.error",
                    Some(handle.peer_id()),
                    request_id,
                    request_kind.command(),
                    error.to_string(),
                );
                return Err(error);
            }
        };
        let mut response = match LegacyResponseCodec::decode_response(
            request_id,
            request_kind,
            response,
            requested_block_hashes.as_ref(),
        ) {
            Ok(response) => response,
            Err(error) => {
                trace_legacy_request_error(
                    &self.trace,
                    "outbound.decode_error",
                    Some(handle.peer_id()),
                    request_id,
                    request_kind.command(),
                    error.to_string(),
                );
                return Err(error.into());
            }
        };
        if request_kind == LegacyRequestKind::Ping {
            // The responder can only acknowledge a Ping; the requester stamps the RTT.
            response = Response::Pong(started_at.elapsed());
        }
        trace_legacy_request_response(
            &self.trace,
            "outbound.response",
            Some(handle.peer_id()),
            request_id,
            request_kind.command(),
            &response,
        );
        Ok(response)
    }
}

fn select_handle(
    handles: &[ZakuraPeerHandle],
    preferred: Option<&ZakuraPeerId>,
) -> Option<ZakuraPeerHandle> {
    // v1 controlled-network routing: source-less chain-sync requests use the
    // first ready Zakura peer. Production spreading/rotation belongs with the
    // future zebrad wiring milestone.
    preferred
        .and_then(|peer_id| {
            handles
                .iter()
                .find(|handle| handle.peer_id() == peer_id)
                .cloned()
        })
        .or_else(|| handles.first().cloned())
}

fn select_fallback_handle(
    handles: &[ZakuraPeerHandle],
    primary: &ZakuraPeerId,
) -> Option<ZakuraPeerHandle> {
    handles
        .iter()
        .find(|handle| handle.peer_id() != primary)
        .cloned()
}

fn all_inventory_missing(response: &Response) -> bool {
    match response {
        Response::Blocks(blocks) => {
            blocks.is_empty() || blocks.iter().all(|block| block.is_missing())
        }
        Response::Transactions(transactions) => {
            transactions.is_empty()
                || transactions
                    .iter()
                    .all(|transaction| transaction.is_missing())
        }
        Response::BlockHashes(hashes) => hashes.is_empty(),
        Response::BlockHeaders(headers) => headers.is_empty(),
        Response::TransactionIds(ids) => ids.is_empty(),
        _ => false,
    }
}

fn trace_legacy_request_start(
    trace: &ZakuraTrace,
    event: &'static str,
    peer: Option<&ZakuraPeerId>,
    request_id: u64,
    request_kind: LegacyRequestKind,
    message_type: u16,
) {
    trace.emit_with(LEGACY_REQUEST_TABLE, |row| {
        insert_trace_str(row, "event", event);
        insert_trace_peer(row, peer);
        insert_trace_u64(row, "request_id", request_id);
        insert_trace_str(row, "request", request_kind.command());
        insert_trace_u64(row, "message_type", u64::from(message_type));
    });
}

fn trace_legacy_request_response(
    trace: &ZakuraTrace,
    event: &'static str,
    peer: Option<&ZakuraPeerId>,
    request_id: u64,
    request: &'static str,
    response: &Response,
) {
    let (response_kind, item_count, missing_count) = response_summary(response);
    trace.emit_with(LEGACY_REQUEST_TABLE, |row| {
        insert_trace_str(row, "event", event);
        insert_trace_peer(row, peer);
        insert_trace_u64(row, "request_id", request_id);
        insert_trace_str(row, "request", request);
        insert_trace_str(row, "response", response_kind);
        insert_trace_u64(row, "item_count", item_count);
        insert_trace_u64(row, "missing_count", missing_count);
    });
}

fn trace_legacy_request_error(
    trace: &ZakuraTrace,
    event: &'static str,
    peer: Option<&ZakuraPeerId>,
    request_id: u64,
    request: &'static str,
    error: String,
) {
    trace.emit_with(LEGACY_REQUEST_TABLE, |row| {
        insert_trace_str(row, "event", event);
        insert_trace_peer(row, peer);
        insert_trace_u64(row, "request_id", request_id);
        insert_trace_str(row, "request", request);
        row.insert("error".to_string(), Value::String(error));
    });
}

fn response_summary(response: &Response) -> (&'static str, u64, u64) {
    match response {
        Response::Blocks(blocks) => (
            "Blocks",
            bounded_u64(blocks.len()),
            bounded_u64(blocks.iter().filter(|block| block.is_missing()).count()),
        ),
        Response::Transactions(transactions) => (
            "Transactions",
            bounded_u64(transactions.len()),
            bounded_u64(
                transactions
                    .iter()
                    .filter(|transaction| transaction.is_missing())
                    .count(),
            ),
        ),
        Response::BlockHashes(hashes) => ("BlockHashes", bounded_u64(hashes.len()), 0),
        Response::BlockHeaders(headers) => ("BlockHeaders", bounded_u64(headers.len()), 0),
        Response::TransactionIds(ids) => ("TransactionIds", bounded_u64(ids.len()), 0),
        Response::Pong(_) => ("Pong", 1, 0),
        Response::Nil => ("Nil", 0, 0),
        response => (response.command(), 0, 0),
    }
}

fn insert_trace_peer(row: &mut Map<String, Value>, peer: Option<&ZakuraPeerId>) {
    row.insert(
        "peer".to_string(),
        peer.map_or(Value::Null, |peer| Value::String(trace_peer_label(peer))),
    );
}

fn insert_trace_str(row: &mut Map<String, Value>, key: &'static str, value: &'static str) {
    row.insert(key.to_string(), Value::String(value.to_string()));
}

fn insert_trace_u64(row: &mut Map<String, Value>, key: &'static str, value: u64) {
    row.insert(key.to_string(), Value::Number(Number::from(value)));
}

fn bounded_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Clone, Debug)]
struct LegacyGossipForwarder {
    broadcast: ZakuraGossipBroadcast,
    /// Short-lived dedup of inventory already handed to the inbound service but not
    /// yet confirmed `mark_seen`. Bounds duplicate work when the service is slow,
    /// not ready, or erroring (see [`LEGACY_GOSSIP_DUPLICATE_COOLDOWN`]).
    attempt_cooldown: FirstSeenCache,
}

impl LegacyGossipForwarder {
    fn new(supervisor: ZakuraSupervisorHandle) -> Self {
        Self {
            broadcast: ZakuraGossipBroadcast::new(supervisor),
            attempt_cooldown: FirstSeenCache::new(
                DEFAULT_FIRST_SEEN_CAPACITY,
                LEGACY_GOSSIP_DUPLICATE_COOLDOWN,
            ),
        }
    }

    async fn unseen(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        self.broadcast.unseen(frame).await
    }

    /// Return the subset of `frame`'s inventory not currently suppressed by a recent
    /// expensive failed attempt. Read-only: the cooldown is populated only by
    /// [`Self::note_failed_attempt`], so fast failures remain immediately retryable.
    async fn fresh_attempt(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        self.attempt_cooldown.unseen(frame).await
    }

    /// Record `frame`'s inventory in the cooldown when a failed attempt consumed at
    /// least [`LEGACY_GOSSIP_EXPENSIVE_ATTEMPT`], so queued duplicates skip re-paying
    /// a slow readiness/call. Cheap failures are left immediately retryable.
    async fn note_failed_attempt(&self, frame: &LegacyGossipFrame, elapsed: Duration) {
        if elapsed >= LEGACY_GOSSIP_EXPENSIVE_ATTEMPT {
            self.attempt_cooldown.record_seen(frame).await;
        }
    }

    async fn mark_seen(&self, frame: &LegacyGossipFrame) {
        self.broadcast.mark_seen(frame).await;
    }

    async fn forward(
        &self,
        peer_id: &ZakuraPeerId,
        frame: LegacyGossipFrame,
    ) -> Result<(), BoxError> {
        self.broadcast.broadcast(frame, Some(peer_id)).await
    }
}

/// Inbound sink that adapts Zakura gossip frames into Zebra's legacy inbound service.
#[derive(Debug)]
pub struct LegacyGossipSink {
    inbound_tx: mpsc::Sender<LegacyInboundWork>,
    outbound: LegacyGossipOutbound,
    trace: ZakuraTrace,
}

impl LegacyGossipSink {
    /// Spawn a bounded inbound worker around the existing legacy inbound service.
    pub fn spawn<Inbound>(inbound: Inbound, supervisor: ZakuraSupervisorHandle) -> Self
    where
        Inbound: Service<Request, Response = Response, Error = BoxError> + Send + Clone + 'static,
        Inbound::Future: Send + 'static,
    {
        Self::spawn_with_trace(inbound, supervisor, ZakuraTrace::noop())
    }

    /// Spawn a bounded inbound worker with a trace emitter.
    pub fn spawn_with_trace<Inbound>(
        inbound: Inbound,
        supervisor: ZakuraSupervisorHandle,
        trace: ZakuraTrace,
    ) -> Self
    where
        Inbound: Service<Request, Response = Response, Error = BoxError> + Send + Clone + 'static,
        Inbound::Future: Send + 'static,
    {
        let (inbound_tx, inbound_rx) = mpsc::channel(LEGACY_GOSSIP_INBOUND_QUEUE);
        tokio::spawn(legacy_gossip_worker(
            inbound,
            inbound_rx,
            LegacyGossipForwarder::new(supervisor.clone()),
            trace.clone(),
        ));
        let outbound = outbound_for_supervisor(&supervisor);
        Self {
            inbound_tx,
            outbound,
            trace,
        }
    }
}

impl LegacyGossipSink {
    fn enqueue_gossip_frame(
        inbound_tx: &mpsc::Sender<LegacyInboundWork>,
        peer_id: ZakuraPeerId,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        let frame = LegacyGossipFrame::decode_frame(frame).map_err(SinkReject::protocol)?;
        match inbound_tx.try_send(LegacyInboundWork::Gossip(LegacyGossipInbound {
            peer_id,
            frame,
        })) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!("legacy gossip inbound queue full: dropping frame");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(SinkReject::local("legacy gossip inbound queue closed"))
            }
        }
    }

    fn deliver(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        if stream_kind != ZAKURA_STREAM_GOSSIP {
            return Ok(());
        }

        Self::enqueue_gossip_frame(&self.inbound_tx, peer_id, frame)
    }

    fn request<'a>(
        &'a self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        request_id: u64,
        max_frame_bytes: u32,
        max_message_bytes: u32,
        frame: Frame,
    ) -> BoxRunFuture<'a, Result<Vec<Frame>, SinkReject>> {
        Box::pin(async move {
            if stream_kind != ZAKURA_STREAM_LEGACY_REQUESTS {
                return Err(SinkReject::protocol(
                    "unsupported legacy request stream kind",
                ));
            }

            let frame = LegacyRequestFrame::decode_frame(frame).map_err(SinkReject::protocol)?;
            let (response_tx, response_rx) = oneshot::channel();
            match self
                .inbound_tx
                .try_send(LegacyInboundWork::Request(LegacyRequestInbound {
                    peer_id: peer_id.clone(),
                    request_id,
                    frame,
                    response_tx,
                })) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return Err(SinkReject::local("legacy request inbound queue full"));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(SinkReject::local("legacy request inbound queue closed"));
                }
            }

            let response = timeout(LEGACY_REQUEST_TIMEOUT, response_rx)
                .await
                .map_err(|_| SinkReject::local("legacy request service timed out"))?
                .map_err(|_| SinkReject::local("legacy request response dropped"))?
                .map_err(SinkReject::local)?;
            trace_legacy_request_response(
                &self.trace,
                "inbound.response",
                Some(&peer_id),
                request_id,
                response.command(),
                &response,
            );
            LegacyResponseCodec::encode_response(
                request_id,
                response,
                max_frame_bytes,
                max_message_bytes,
            )
            .map_err(SinkReject::local)
        })
    }
}

impl ZakuraService for LegacyGossipSink {
    fn name(&self) -> &'static str {
        "legacy-gossip"
    }

    fn streams(&self) -> &[Stream] {
        legacy_gossip_streams()
    }

    fn add_peer(&self, mut peer: Peer) {
        let Some((mut recv, send)) = peer.take_stream(ZAKURA_STREAM_GOSSIP) else {
            return;
        };
        let outbound = self.outbound.clone();
        let inbound_tx = self.inbound_tx.clone();
        let peer_id = peer.id.clone();
        let cancel_token = peer.cancel_token();
        let session = LegacyGossipPeerSession::new(peer_id.clone(), send);

        outbound.insert(session.clone());
        let replay_task_peer_id = peer_id.clone();
        let replay_panic_peer_id = replay_task_peer_id.clone();
        let replay_panic_outbound = outbound.clone();
        let replay_panic_cancel = cancel_token.clone();
        spawn_supervised_peer_task(
            replay_task_peer_id,
            || {},
            move || {
                replay_panic_cancel.cancel();
                replay_panic_outbound.remove(&replay_panic_peer_id);
            },
            {
                let outbound = outbound.clone();
                let session = session.clone();
                async move {
                    if let Err(error) = outbound.replay_latest_block_to_peer(session).await {
                        debug!(?error, "latest Zakura block gossip replay failed");
                    }
                }
            },
        );

        let recv_task_peer_id = peer_id.clone();
        let recv_panic_peer_id = recv_task_peer_id.clone();
        let recv_panic_outbound = outbound.clone();
        let recv_panic_cancel = cancel_token.clone();
        spawn_supervised_peer_task(
            recv_task_peer_id,
            || {},
            move || {
                recv_panic_cancel.cancel();
                recv_panic_outbound.remove(&recv_panic_peer_id);
            },
            async move {
                loop {
                    let frame = tokio::select! {
                        _ = cancel_token.cancelled() => {
                            outbound.remove(&peer_id);
                            return;
                        }
                        frame = recv.recv() => {
                            let Some(frame) = frame else {
                                outbound.remove(&peer_id);
                                return;
                            };
                            frame
                        }
                    };

                    #[cfg(test)]
                    if should_panic_legacy_gossip_recv_loop(&peer_id) {
                        panic!("injected legacy gossip recv-loop panic after state registration");
                    }

                    match Self::enqueue_gossip_frame(&inbound_tx, peer_id.clone(), frame) {
                        Ok(()) => {}
                        Err(SinkReject::Protocol(error)) => {
                            debug!(
                                ?error,
                                ?peer_id,
                                "legacy gossip stream rejected protocol-invalid frame"
                            );
                            cancel_token.cancel();
                            outbound.remove(&peer_id);
                            return;
                        }
                        Err(SinkReject::Local(error)) => {
                            debug!(?error, ?peer_id, "legacy gossip inbound queue closed");
                            return;
                        }
                    }
                }
            },
        );
    }

    fn remove_peer(&self, peer: &ZakuraPeerId) {
        self.outbound.remove(peer);
    }

    fn deliver_frame(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        self.deliver(peer_id, stream_kind, frame)
    }

    fn as_request_response(&self) -> Option<&dyn RequestResponseService> {
        Some(self)
    }
}

impl RequestResponseService for LegacyGossipSink {
    fn request_frame<'a>(
        &'a self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        request_id: u64,
        max_frame_bytes: u32,
        max_message_bytes: u32,
        frame: Frame,
    ) -> BoxRunFuture<'a, Result<Vec<Frame>, SinkReject>> {
        self.request(
            peer_id,
            stream_kind,
            request_id,
            max_frame_bytes,
            max_message_bytes,
            frame,
        )
    }
}

#[derive(Debug)]
enum LegacyInboundWork {
    Gossip(LegacyGossipInbound),
    Request(LegacyRequestInbound),
}

#[derive(Debug)]
struct LegacyGossipInbound {
    peer_id: ZakuraPeerId,
    frame: LegacyGossipFrame,
}

#[derive(Debug)]
struct LegacyRequestInbound {
    peer_id: ZakuraPeerId,
    request_id: u64,
    frame: LegacyRequestFrame,
    response_tx: oneshot::Sender<Result<Response, BoxError>>,
}

async fn legacy_gossip_worker<Inbound>(
    mut inbound: Inbound,
    mut inbound_rx: mpsc::Receiver<LegacyInboundWork>,
    forwarder: LegacyGossipForwarder,
    trace: ZakuraTrace,
) where
    Inbound: Service<Request, Response = Response, Error = BoxError> + Send + Clone + 'static,
    Inbound::Future: Send + 'static,
{
    let request_permits = Arc::new(Semaphore::new(LEGACY_REQUEST_IN_FLIGHT_LIMIT));

    while let Some(work) = inbound_rx.recv().await {
        match work {
            LegacyInboundWork::Gossip(gossip) => {
                handle_legacy_gossip(&mut inbound, gossip, &forwarder).await;
            }
            LegacyInboundWork::Request(request) => {
                let Ok(permit) = request_permits.clone().try_acquire_owned() else {
                    let _ = request.response_tx.send(Err(
                        "legacy request inbound concurrency limit reached".into(),
                    ));
                    continue;
                };
                tokio::spawn(handle_legacy_request(
                    inbound.clone(),
                    request,
                    permit,
                    trace.clone(),
                ));
            }
        }
    }
}

async fn handle_legacy_gossip<Inbound>(
    inbound: &mut Inbound,
    gossip: LegacyGossipInbound,
    forwarder: &LegacyGossipForwarder,
) where
    Inbound: Service<Request, Response = Response, Error = BoxError> + Send + 'static,
    Inbound::Future: Send + 'static,
{
    let Some(unseen_frame) = forwarder.unseen(&gossip.frame).await else {
        return;
    };
    // Skip inventory whose recent service attempt was expensive but did not succeed.
    // The not-ready, call-error, and call-timeout paths below all return without
    // `mark_seen`, so without this an authenticated peer could replay the same valid
    // advertisement while the inbound service is slow/erroring and make the serial
    // worker re-pay the full 30s+30s readiness/call budget for every queued
    // duplicate. Fast failures are not recorded, so a transient blip stays retryable.
    let Some(unseen_frame) = forwarder.fresh_attempt(&unseen_frame).await else {
        debug!("legacy gossip duplicate suppressed after recent expensive attempt");
        return;
    };
    let request = unseen_frame.clone().into_request(gossip.peer_id.clone());

    let started = Instant::now();
    let ready = timeout(LEGACY_GOSSIP_SERVICE_TIMEOUT, inbound.ready()).await;
    let Ok(Ok(service)) = ready else {
        debug!("legacy gossip inbound service was not ready");
        forwarder
            .note_failed_attempt(&unseen_frame, started.elapsed())
            .await;
        return;
    };

    match timeout(LEGACY_GOSSIP_SERVICE_TIMEOUT, service.call(request)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            debug!(?error, "legacy gossip inbound service call failed");
            forwarder
                .note_failed_attempt(&unseen_frame, started.elapsed())
                .await;
            return;
        }
        Err(_) => {
            debug!("legacy gossip inbound service call timed out");
            forwarder
                .note_failed_attempt(&unseen_frame, started.elapsed())
                .await;
            return;
        }
    }

    forwarder.mark_seen(&unseen_frame).await;
    if let Err(error) = forwarder.forward(&gossip.peer_id, unseen_frame).await {
        debug!(?error, "legacy gossip forward failed");
    }
}

async fn handle_legacy_request<Inbound>(
    mut inbound: Inbound,
    request: LegacyRequestInbound,
    _permit: OwnedSemaphorePermit,
    trace: ZakuraTrace,
) where
    Inbound: Service<Request, Response = Response, Error = BoxError> + Send + 'static,
    Inbound::Future: Send + 'static,
{
    let command = request.frame.to_string();
    let peer_id = request.peer_id.clone();
    let request_id = request.request_id;
    let request_kind = request.frame.kind();
    let mut response_tx = request.response_tx;
    let Some(legacy_request) = request.frame.into_service_request() else {
        // Inbound legacy Ping is handled locally; the requester measures round-trip time.
        let _ = response_tx.send(Ok(Response::Pong(Duration::ZERO)));
        return;
    };
    trace_legacy_request_start(
        &trace,
        "inbound.request",
        Some(&peer_id),
        request_id,
        request_kind,
        request_kind.message_type(),
    );

    let service_work = async move {
        let ready = timeout(LEGACY_REQUEST_TIMEOUT, inbound.ready()).await;
        match ready {
            Ok(Ok(service)) => timeout(LEGACY_REQUEST_TIMEOUT, service.call(legacy_request))
                .await
                .map_err(|_| -> BoxError { format!("{command} inbound service timed out").into() })
                .and_then(|response| response),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(format!("{command} inbound service readiness timed out").into()),
        }
    };

    // Tie the spawned handler's lifetime to the request stream. The request-stream
    // side (`LegacyGossipSink::request`) waits only LEGACY_REQUEST_TIMEOUT for the
    // oneshot result and then drops the receiver; the peer/connection going away
    // drops it too. In either case `closed()` resolves, so abort the in-flight
    // service work and release the permit instead of letting this orphaned handler
    // keep one of LEGACY_REQUEST_IN_FLIGHT_LIMIT permits (and keep doing backend
    // work) for up to another full readiness + service-call timeout.
    let result = tokio::select! {
        biased;
        () = response_tx.closed() => {
            debug!(
                ?peer_id,
                request_id,
                "legacy request handler aborted: response receiver dropped before service completed"
            );
            return;
        }
        result = service_work => result,
    };

    if let Err(error) = &result {
        trace_legacy_request_error(
            &trace,
            "inbound.error",
            Some(&peer_id),
            request_id,
            request_kind.command(),
            error.to_string(),
        );
    }

    if response_tx.send(result).is_err() {
        debug!(
            ?peer_id,
            request_id, "legacy request response receiver dropped before service completed"
        );
    }
}

#[derive(Clone, Debug)]
struct FirstSeenCache {
    inner: std::sync::Arc<Mutex<FirstSeenCacheInner>>,
}

impl FirstSeenCache {
    fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(FirstSeenCacheInner {
                capacity: capacity.max(1),
                ttl,
                entries: HashMap::new(),
                order: VecDeque::new(),
            })),
        }
    }

    async fn record_unseen(
        &self,
        keys: impl IntoIterator<Item = InventoryKey>,
    ) -> Vec<InventoryKey> {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        inner.prune(now);
        let mut recorded = Vec::new();
        for key in keys {
            if inner.insert(key, now) {
                recorded.push(key);
            }
        }
        recorded
    }

    async fn record_first_seen(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        let unseen = self.unseen(frame).await?;
        self.record_seen(&unseen).await;
        Some(unseen)
    }

    async fn unseen(&self, frame: &LegacyGossipFrame) -> Option<LegacyGossipFrame> {
        match frame {
            LegacyGossipFrame::AdvertiseBlock(hash) => {
                (!self.contains(InventoryKey::Block(*hash)).await)
                    .then_some(LegacyGossipFrame::AdvertiseBlock(*hash))
            }
            LegacyGossipFrame::AdvertiseTransactionIds(ids) => {
                let unseen = self.unrecorded_transaction_ids(ids.iter().copied()).await;
                (!unseen.is_empty()).then_some(LegacyGossipFrame::AdvertiseTransactionIds(unseen))
            }
        }
    }

    async fn record_seen(&self, frame: &LegacyGossipFrame) {
        match frame {
            LegacyGossipFrame::AdvertiseBlock(hash) => {
                self.record_unseen([InventoryKey::Block(*hash)]).await;
            }
            LegacyGossipFrame::AdvertiseTransactionIds(ids) => {
                self.record_unseen(ids.iter().copied().map(InventoryKey::Transaction))
                    .await;
            }
        }
    }

    async fn contains(&self, key: InventoryKey) -> bool {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        inner.prune(now);
        inner.entries.contains_key(&key)
    }

    async fn unrecorded_transaction_ids(
        &self,
        ids: impl IntoIterator<Item = UnminedTxId>,
    ) -> Vec<UnminedTxId> {
        let mut inner = self.inner.lock().await;
        inner.prune(Instant::now());
        let mut unseen = Vec::new();
        for tx_id in ids {
            if !inner
                .entries
                .contains_key(&InventoryKey::Transaction(tx_id))
            {
                unseen.push(tx_id);
            }
        }
        unseen
    }
}

#[derive(Debug)]
struct FirstSeenCacheInner {
    capacity: usize,
    ttl: Duration,
    entries: HashMap<InventoryKey, Instant>,
    order: VecDeque<InventoryKey>,
}

impl FirstSeenCacheInner {
    fn insert(&mut self, key: InventoryKey, now: Instant) -> bool {
        if self.entries.contains_key(&key) {
            return false;
        }
        self.entries.insert(key, now);
        self.order.push_back(key);
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        true
    }

    fn prune(&mut self, now: Instant) {
        while let Some(key) = self.order.front().copied() {
            let Some(seen_at) = self.entries.get(&key).copied() else {
                self.order.pop_front();
                continue;
            };
            if now.saturating_duration_since(seen_at) < self.ttl {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&key);
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
enum InventoryKey {
    Block(block::Hash),
    Transaction(UnminedTxId),
}

/// Errors produced by the legacy gossip adapter.
#[derive(Debug, Error)]
pub enum LegacyGossipError {
    /// The request is intentionally outside this phase's gossip-only scope.
    #[error("unsupported legacy gossip request: {0}")]
    UnsupportedRequest(&'static str),
    /// A peer used nonzero flags before any flags are defined.
    #[error("unsupported legacy gossip flags: {0}")]
    UnsupportedFlags(u16),
    /// A peer sent an unknown gossip message type.
    #[error("unknown legacy gossip message type: {0}")]
    UnknownMessageType(u16),
    /// Transaction advertisements must not be empty.
    #[error("empty transaction-id advertisement")]
    EmptyTransactionAdvertisement,
    /// A peer requested or responded with too many inventory items.
    #[error("too many inventory items in legacy request/response: {0}")]
    TooManyInventoryItems(usize),
    /// A peer requested too many block locator hashes.
    #[error("too many block locator hashes in legacy request: {0}")]
    TooManyBlockLocatorHashes(usize),
    /// A peer responded with too many block headers.
    #[error("too many block headers in legacy response: {0}")]
    TooManyHeaders(usize),
    /// Transaction gossip payload included non-transaction inventory.
    #[error("transaction-id advertisement contained non-transaction inventory")]
    NonTransactionInventory,
    /// Payload had bytes after the decoded message.
    #[error("trailing bytes after legacy gossip payload")]
    TrailingBytes,
    /// A response frame echoed a different request id.
    #[error("wrong legacy request id in response: expected {expected}, got {actual}")]
    WrongRequestId {
        /// Expected request id.
        expected: u64,
        /// Actual request id in the response frame.
        actual: u64,
    },
    /// A chunked response ended before its final chunk.
    #[error("incomplete legacy response chunk")]
    IncompleteResponseChunk,
    /// A response payload was too short to contain its fixed header.
    #[error("truncated legacy response")]
    TruncatedResponse,
    /// A response exceeded the protocol message size.
    #[error("oversized legacy response: {0} bytes")]
    OversizedResponse(usize),
    /// The encoded response would exceed the responder-side aggregate budget.
    #[error("legacy response exceeded responder aggregate budget: {0} bytes")]
    ResponseAggregateBudget(usize),
    /// The legacy service returned an unexpected response variant.
    #[error("unexpected legacy response: {0}")]
    UnexpectedResponse(&'static str),
    /// A request that requires an acknowledgement received none.
    #[error("missing legacy response: {0}")]
    MissingResponse(&'static str),
    /// The peer returned a block we did not request, so the response is not bound
    /// to the requested hash. Treated as a peer fault.
    #[error("legacy block response contained an unrequested block: {0:?}")]
    UnsolicitedBlock(block::Hash),
    /// Zcash serialization failed.
    #[error(transparent)]
    Serialization(#[from] SerializationError),
    /// Local buffer serialization failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Integer conversion failed while checking a peer-controlled bound.
    #[error(transparent)]
    Integer(#[from] std::num::TryFromIntError),
}

impl fmt::Display for LegacyGossipFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdvertiseBlock(_) => f.write_str("AdvertiseBlock"),
            Self::AdvertiseTransactionIds(ids) => {
                write!(f, "AdvertiseTransactionIds({})", ids.len())
            }
        }
    }
}

impl fmt::Display for LegacyRequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlocksByHash(hashes) => write!(f, "BlocksByHash({})", hashes.len()),
            Self::TransactionsById(ids) => write!(f, "TransactionsById({})", ids.len()),
            Self::FindBlocks { known_blocks, stop } => write!(
                f,
                "FindBlocks {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                stop.is_some()
            ),
            Self::FindHeaders { known_blocks, stop } => write!(
                f,
                "FindHeaders {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                stop.is_some()
            ),
            Self::MempoolTransactionIds => f.write_str("MempoolTransactionIds"),
            Self::Ping => f.write_str("Ping"),
            Self::PushTransaction(_) => f.write_str("PushTransaction"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        task::{Context, Poll},
    };

    use futures::FutureExt;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tower::ServiceExt;
    use zebra_chain::{
        parameters::NetworkUpgrade,
        serialization::{ZcashDeserialize, ZcashSerialize},
        transaction::{self, LockTime, Transaction, WtxId},
    };
    use zebra_test::vectors::BLOCK_TESTNET_141042_BYTES;

    use crate::zakura::{
        framed_channel,
        testkit::{HostilePeer, ZakuraTestNode},
        ZAKURA_CAP_LEGACY_GOSSIP,
    };

    fn block_hash(byte: u8) -> block::Hash {
        block::Hash([byte; 32])
    }

    fn legacy_tx_id(byte: u8) -> UnminedTxId {
        UnminedTxId::from_legacy_id(transaction::Hash([byte; 32]))
    }

    fn witnessed_tx_id(byte: u8) -> UnminedTxId {
        UnminedTxId::from(WtxId {
            id: transaction::Hash([byte; 32]),
            auth_digest: transaction::AuthDigest::from([byte.wrapping_add(1); 32]),
        })
    }

    fn empty_v5_transaction(byte: u8) -> Transaction {
        Transaction::V5 {
            network_upgrade: NetworkUpgrade::Nu5,
            lock_time: LockTime::min_lock_time_timestamp(),
            expiry_height: block::Height(u32::from(byte)),
            inputs: Vec::new(),
            outputs: Vec::new(),
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        }
    }

    fn encoded_count(count: usize) -> Vec<u8> {
        CompactSizeMessage::try_from(count)
            .expect("test count is within CompactSizeMessage bounds")
            .zcash_serialize_to_vec()
            .expect("compact size serializes")
    }

    #[derive(Clone, Debug)]
    struct RequestRecorder {
        tx: tokio::sync::mpsc::UnboundedSender<Request>,
    }

    impl Service<Request> for RequestRecorder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            match self.tx.send(request) {
                Ok(()) => std::future::ready(Ok(Response::Nil)),
                Err(_) => std::future::ready(Err("request recorder receiver dropped".into())),
            }
        }
    }

    #[derive(Clone, Debug)]
    struct FailsOnceRecorder {
        attempts: Arc<AtomicUsize>,
        tx: tokio::sync::mpsc::UnboundedSender<Request>,
    }

    impl Service<Request> for FailsOnceRecorder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return std::future::ready(Err("intentional transient inbound failure".into()));
            }

            match self.tx.send(request) {
                Ok(()) => std::future::ready(Ok(Response::Nil)),
                Err(_) => std::future::ready(Err("request recorder receiver dropped".into())),
            }
        }
    }

    #[derive(Clone, Debug)]
    struct InventoryResponder {
        transaction: UnminedTx,
    }

    impl Service<Request> for InventoryResponder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            let response = match request {
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    Response::Blocks(hashes.into_iter().map(InventoryResponse::Missing).collect())
                }
                Request::TransactionsById(ids) | Request::TransactionsByIdFrom { ids, .. } => {
                    Response::Transactions(
                        ids.into_iter()
                            .map(|id| {
                                if id == self.transaction.id {
                                    InventoryResponse::Available((self.transaction.clone(), None))
                                } else {
                                    InventoryResponse::Missing(id)
                                }
                            })
                            .collect(),
                    )
                }
                request => {
                    return std::future::ready(Err(format!(
                        "unexpected inventory request: {request:?}"
                    )
                    .into()));
                }
            };
            std::future::ready(Ok(response))
        }
    }

    #[derive(Clone, Debug)]
    struct BlockInventoryResponder {
        block: Arc<Block>,
    }

    impl Service<Request> for BlockInventoryResponder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            let response = match request {
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    Response::Blocks(
                        hashes
                            .into_iter()
                            .map(|hash| {
                                if hash == self.block.hash() {
                                    InventoryResponse::Available((self.block.clone(), None))
                                } else {
                                    InventoryResponse::Missing(hash)
                                }
                            })
                            .collect(),
                    )
                }
                request => {
                    return std::future::ready(Err(format!(
                        "unexpected block inventory request: {request:?}"
                    )
                    .into()));
                }
            };
            std::future::ready(Ok(response))
        }
    }

    #[derive(Clone, Debug)]
    struct SlowRequestThenRecorder {
        release: tokio::sync::watch::Receiver<bool>,
        tx: tokio::sync::mpsc::UnboundedSender<Request>,
    }

    impl Service<Request> for SlowRequestThenRecorder {
        type Response = Response;
        type Error = BoxError;
        type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            match request {
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    let mut release = self.release.clone();
                    async move {
                        while !*release.borrow() {
                            if release.changed().await.is_err() {
                                break;
                            }
                        }
                        Ok(Response::Blocks(
                            hashes.into_iter().map(InventoryResponse::Missing).collect(),
                        ))
                    }
                    .boxed()
                }
                request => {
                    let tx = self.tx.clone();
                    async move {
                        tx.send(request)?;
                        Ok(Response::Nil)
                    }
                    .boxed()
                }
            }
        }
    }

    /// Always ready, but every `call` future is pending forever. Models a slow or
    /// backpressured inbound service whose work outlives the request-stream timeout.
    #[derive(Clone, Debug)]
    struct NeverCompletesService;

    impl Service<Request> for NeverCompletesService {
        type Response = Response;
        type Error = BoxError;
        type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: Request) -> Self::Future {
            std::future::pending().boxed()
        }
    }

    /// Always ready, but every `call` future is pending forever and counts
    /// invocations. Models a slow/backpressured inbound service whose calls hit
    /// `LEGACY_GOSSIP_SERVICE_TIMEOUT`, so `handle_legacy_gossip` returns without
    /// `mark_seen`, exposing how many times duplicate gossip frames reach the
    /// expensive readiness/call path.
    #[derive(Clone, Debug)]
    struct CountingPendingService {
        calls: Arc<AtomicUsize>,
    }

    impl Service<Request> for CountingPendingService {
        type Response = Response;
        type Error = BoxError;
        type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: Request) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().boxed()
        }
    }

    #[derive(Clone, Debug)]
    struct RecordingInventoryResponder {
        transaction: Option<UnminedTx>,
        tx: tokio::sync::mpsc::UnboundedSender<Request>,
    }

    impl Service<Request> for RecordingInventoryResponder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            if let Err(error) = self.tx.send(request.clone()) {
                return std::future::ready(Err(Box::new(error)));
            }

            let response = match request {
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    Response::Blocks(hashes.into_iter().map(InventoryResponse::Missing).collect())
                }
                Request::TransactionsById(ids) | Request::TransactionsByIdFrom { ids, .. } => {
                    Response::Transactions(
                        ids.into_iter()
                            .map(|id| match &self.transaction {
                                Some(transaction) if id == transaction.id => {
                                    InventoryResponse::Available((transaction.clone(), None))
                                }
                                _ => InventoryResponse::Missing(id),
                            })
                            .collect(),
                    )
                }
                request => {
                    return std::future::ready(Err(format!(
                        "unexpected inventory request: {request:?}"
                    )
                    .into()));
                }
            };
            std::future::ready(Ok(response))
        }
    }

    #[derive(Clone, Debug)]
    struct NormalNetworkResponder {
        block: Arc<Block>,
        transaction: UnminedTx,
        pushed_tx: tokio::sync::mpsc::UnboundedSender<UnminedTxId>,
    }

    impl NormalNetworkResponder {
        fn header(&self) -> block::CountedHeader {
            block::CountedHeader {
                header: self.block.header.clone(),
            }
        }
    }

    impl Service<Request> for NormalNetworkResponder {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            let response = match request {
                Request::FindBlocks { .. } => Response::BlockHashes(vec![self.block.hash()]),
                Request::FindHeaders { .. } => Response::BlockHeaders(vec![self.header()]),
                Request::MempoolTransactionIds => {
                    Response::TransactionIds(vec![self.transaction.id])
                }
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    Response::Blocks(
                        hashes
                            .into_iter()
                            .map(|hash| {
                                if hash == self.block.hash() {
                                    InventoryResponse::Available((self.block.clone(), None))
                                } else {
                                    InventoryResponse::Missing(hash)
                                }
                            })
                            .collect(),
                    )
                }
                Request::PushTransaction(transaction) => {
                    if let Err(error) = self.pushed_tx.send(transaction.id) {
                        return std::future::ready(Err(Box::new(error)));
                    }
                    Response::Nil
                }
                request => {
                    return std::future::ready(Err(format!(
                        "unexpected normal-network request: {request:?}"
                    )
                    .into()));
                }
            };
            std::future::ready(Ok(response))
        }
    }

    async fn legacy_node(
        seed: u64,
    ) -> Result<(ZakuraTestNode, UnboundedReceiver<Request>), BoxError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let node = ZakuraTestNode::builder(seed)
            .service_from_supervisor(move |supervisor| {
                Arc::new(LegacyGossipSink::spawn(RequestRecorder { tx }, supervisor))
            })
            .spawn()
            .await?;
        Ok((node, rx))
    }

    async fn inventory_node(seed: u64, transaction: UnminedTx) -> Result<ZakuraTestNode, BoxError> {
        let node = ZakuraTestNode::builder(seed)
            .service_from_supervisor(move |supervisor| {
                Arc::new(LegacyGossipSink::spawn(
                    InventoryResponder { transaction },
                    supervisor,
                ))
            })
            .spawn()
            .await?;
        Ok(node)
    }

    async fn block_inventory_node(
        seed: u64,
        block: Arc<Block>,
    ) -> Result<ZakuraTestNode, BoxError> {
        let node = ZakuraTestNode::builder(seed)
            .service_from_supervisor(move |supervisor| {
                Arc::new(LegacyGossipSink::spawn(
                    BlockInventoryResponder { block },
                    supervisor,
                ))
            })
            .spawn()
            .await?;
        Ok(node)
    }

    async fn recording_inventory_node(
        seed: u64,
        transaction: Option<UnminedTx>,
    ) -> Result<(ZakuraTestNode, UnboundedReceiver<Request>), BoxError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let node = ZakuraTestNode::builder(seed)
            .service_from_supervisor(move |supervisor| {
                Arc::new(LegacyGossipSink::spawn(
                    RecordingInventoryResponder { transaction, tx },
                    supervisor,
                ))
            })
            .spawn()
            .await?;
        Ok((node, rx))
    }

    async fn normal_network_node(
        seed: u64,
        block: Arc<Block>,
        transaction: UnminedTx,
    ) -> Result<
        (
            ZakuraTestNode,
            tokio::sync::mpsc::UnboundedReceiver<UnminedTxId>,
        ),
        BoxError,
    > {
        let (pushed_tx, pushed_rx) = tokio::sync::mpsc::unbounded_channel();
        let node = ZakuraTestNode::builder(seed)
            // Multi-node gossip topologies dial several loopback peers from one
            // node, so opt out of the production per-IP cap (1) that the default
            // test node now enforces; these tests exercise gossip routing, not
            // the per-IP admission gate.
            .max_connections_per_ip(8)
            .service_from_supervisor(move |supervisor| {
                Arc::new(LegacyGossipSink::spawn(
                    NormalNetworkResponder {
                        block,
                        transaction,
                        pushed_tx,
                    },
                    supervisor,
                ))
            })
            .spawn()
            .await?;
        Ok((node, pushed_rx))
    }

    async fn node_peer_id(node: &ZakuraTestNode) -> Result<ZakuraPeerId, BoxError> {
        Ok(ZakuraPeerId::new(
            node.node_addr().await.node_id.as_bytes().to_vec(),
        )?)
    }

    async fn recv_request(rx: &mut UnboundedReceiver<Request>) -> Result<Request, BoxError> {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .map_err(|_| -> BoxError { "timed out waiting for request".into() })?
            .ok_or_else(|| "request recorder closed".into())
    }

    async fn recv_pushed_tx_id(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<UnminedTxId>,
    ) -> Result<UnminedTxId, BoxError> {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .map_err(|_| -> BoxError { "timed out waiting for pushed transaction".into() })?
            .ok_or_else(|| "pushed transaction recorder closed".into())
    }

    fn legacy_gossip_peer(
        peer_id: ZakuraPeerId,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> (Peer, FramedSend) {
        let (peer_send, service_recv) = framed_channel(8);
        let (service_send, _peer_recv) = framed_channel(8);
        let peer = Peer::new(
            peer_id,
            None,
            ZAKURA_CAP_LEGACY_GOSSIP,
            HashMap::from([(ZAKURA_STREAM_GOSSIP, (service_recv, service_send))]),
            cancel_token,
        );
        (peer, peer_send)
    }

    async fn wait_for_legacy_gossip_panic_cleanup(
        outbound: &LegacyGossipOutbound,
        peer_id: &ZakuraPeerId,
        cancel_token: &tokio_util::sync::CancellationToken,
    ) -> Result<(), BoxError> {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cancel_token.is_cancelled() && !outbound.contains(peer_id) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| -> BoxError { "timed out waiting for legacy gossip panic cleanup".into() })
    }

    async fn wait_registered_count(node: &ZakuraTestNode, count: usize) -> Result<(), BoxError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if node.supervisor().registered_ids().await.len() == count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| -> BoxError { "timed out waiting for peer registration count".into() })
    }

    /// Regression for `claude-legacy-request-orphaned-handler-permits`.
    ///
    /// The request-stream side (`LegacyGossipSink::request`) waits only
    /// `LEGACY_REQUEST_TIMEOUT` for the oneshot result and then drops the receiver;
    /// the peer disconnecting drops it too. The spawned `handle_legacy_request` task
    /// must not keep its in-flight permit (one of `LEGACY_REQUEST_IN_FLIGHT_LIMIT`)
    /// or its backend service work alive after that. Before the fix it stayed blocked
    /// in `service.call` for up to another full `LEGACY_REQUEST_TIMEOUT`, so an
    /// attacker driving slow inbound work could occupy all 64 permits.
    #[tokio::test(start_paused = true)]
    async fn legacy_request_handler_releases_permit_when_request_stream_drops_receiver() {
        let permits = Arc::new(Semaphore::new(LEGACY_REQUEST_IN_FLIGHT_LIMIT));
        let permit = permits
            .clone()
            .try_acquire_owned()
            .expect("a permit is available");
        assert_eq!(
            permits.available_permits(),
            LEGACY_REQUEST_IN_FLIGHT_LIMIT - 1,
            "one permit is held while the handler runs"
        );

        let (response_tx, response_rx) = oneshot::channel();
        let request = LegacyRequestInbound {
            peer_id: ZakuraPeerId::new(vec![7u8; 32]).expect("valid peer id"),
            request_id: 1,
            // A non-Ping request so the handler drives the inbound service.
            frame: LegacyRequestFrame::MempoolTransactionIds,
            response_tx,
        };

        let handler = tokio::spawn(handle_legacy_request(
            NeverCompletesService,
            request,
            permit,
            ZakuraTrace::noop(),
        ));

        // Let the handler enter the (never-completing) service call, then model the
        // request-stream side giving up: it drops the oneshot receiver.
        tokio::task::yield_now().await;
        drop(response_rx);

        // The handler must observe the dropped receiver, abort the service work, and
        // release the permit promptly. Without the fix this times out because the
        // handler stays blocked in `service.call` until `LEGACY_REQUEST_TIMEOUT`.
        tokio::time::timeout(Duration::from_secs(5), handler)
            .await
            .expect("handler aborts promptly after the request stream drops the receiver")
            .expect("handler task does not panic");

        assert_eq!(
            permits.available_permits(),
            LEGACY_REQUEST_IN_FLIGHT_LIMIT,
            "the in-flight permit must be released once the handler aborts"
        );
    }

    #[test]
    fn block_gossip_round_trips_and_rejects_trailing_bytes() {
        let frame = LegacyGossipFrame::AdvertiseBlock(block_hash(1));
        let mut encoded = frame.encode_frame().expect("frame encodes");
        assert_eq!(
            LegacyGossipFrame::decode_frame(encoded.clone()).expect("frame decodes"),
            frame
        );

        encoded.payload.push(0);
        assert!(matches!(
            LegacyGossipFrame::decode_frame(encoded),
            Err(LegacyGossipError::TrailingBytes)
        ));
    }

    #[tokio::test]
    async fn adapter_gossips_block_and_tx_ids_between_two_nodes() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let (node_a, mut rx_a) = legacy_node(11).await?;
        let (node_b, mut rx_b) = legacy_node(12).await?;
        node_a
            .connect_native(&node_b, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let mut adapter = LegacyGossipAdapter::new(node_a.supervisor());
        let block_hash = block_hash(9);
        adapter
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(block_hash))
            .await?;

        match recv_request(&mut rx_b).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(hash, block_hash);
                assert_eq!(peer_id, a_peer_id);
            }
            request => panic!("unexpected request: {request:?}"),
        }
        assert!(rx_a.try_recv().is_err());

        let ids = HashSet::from([legacy_tx_id(10), witnessed_tx_id(11)]);
        adapter
            .ready()
            .await?
            .call(Request::AdvertiseTransactionIds(ids.clone(), None))
            .await?;

        match recv_request(&mut rx_b).await? {
            Request::AdvertiseTransactionIds(received, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(received, ids);
                assert_eq!(peer_id, a_peer_id);
            }
            request => panic!("unexpected request: {request:?}"),
        }

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_adapter_fetches_missing_block_from_advertiser() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(1));
        let node_a = inventory_node(61, transaction).await?;
        let node_b = ZakuraTestNode::builder(62).spawn().await?;
        node_b
            .connect_native(&node_a, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;
        let hash = block_hash(90);

        let adapter = LegacyRequestAdapter::new(node_b.supervisor());
        let response = adapter
            .request_from_source(
                Request::BlocksByHash(HashSet::from([hash])),
                Some(PeerSource::Zakura(a_peer_id)),
            )
            .await?;

        match response {
            Response::Blocks(blocks) => {
                assert_eq!(blocks, vec![InventoryResponse::Missing(hash)]);
            }
            response => panic!("unexpected response: {response:?}"),
        }

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_adapter_fetches_available_multi_chunk_block() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let serialized_len = block.zcash_serialize_to_vec()?.len();
        assert!(
            serialized_len > LEGACY_RESPONSE_CHUNK_BYTES,
            "test block must span multiple response chunks"
        );

        let node_a = block_inventory_node(68, block.clone()).await?;
        let node_b = ZakuraTestNode::builder(69).spawn().await?;
        node_b
            .connect_native(&node_a, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let adapter = LegacyRequestAdapter::new(node_b.supervisor());
        let response = adapter
            .request_from_source(
                Request::BlocksByHash(HashSet::from([block.hash()])),
                Some(PeerSource::Zakura(a_peer_id)),
            )
            .await?;

        let Response::Blocks(blocks) = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(matches!(
            blocks.as_slice(),
            [InventoryResponse::Available((received, None))] if received.hash() == block.hash()
        ));

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_adapter_fetches_available_and_missing_transactions() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(2));
        let available_id = transaction.id;
        let missing_id = witnessed_tx_id(99);
        let node_a = inventory_node(63, transaction.clone()).await?;
        let node_b = ZakuraTestNode::builder(64).spawn().await?;
        node_b
            .connect_native(&node_a, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let adapter = LegacyRequestAdapter::new(node_b.supervisor());
        let response = adapter
            .request_from_source(
                Request::TransactionsById(HashSet::from([available_id, missing_id])),
                Some(PeerSource::Zakura(a_peer_id)),
            )
            .await?;

        let Response::Transactions(transactions) = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(transactions.iter().any(|response| {
            matches!(
                response,
                InventoryResponse::Available((tx, None)) if tx.id == transaction.id
            )
        }));
        assert!(transactions.iter().any(
            |response| matches!(response, InventoryResponse::Missing(id) if *id == missing_id)
        ));

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_adapter_drives_mock_chain_sync_progress_over_zakura() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let transaction = UnminedTx::from(empty_v5_transaction(4));
        let (node_a, _pushed_rx) = normal_network_node(81, block.clone(), transaction).await?;
        let node_b = ZakuraTestNode::builder(82).spawn().await?;
        node_b
            .connect_native(&node_a, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let adapter = LegacyRequestAdapter::new(node_b.supervisor());
        let mut local_chain = vec![block_hash(1)];
        let find_headers = adapter
            .request_from_source(
                Request::FindHeaders {
                    known_blocks: local_chain.clone(),
                    stop: Some(block.hash()),
                },
                Some(PeerSource::Zakura(a_peer_id.clone())),
            )
            .await?;
        let Response::BlockHeaders(headers) = find_headers else {
            panic!("unexpected FindHeaders response: {find_headers:?}");
        };
        assert!(matches!(
            headers.as_slice(),
            [header] if header.header == block.header
        ));

        let find_blocks = adapter
            .request_from_source(
                Request::FindBlocks {
                    known_blocks: local_chain.clone(),
                    stop: Some(block.hash()),
                },
                Some(PeerSource::Zakura(a_peer_id.clone())),
            )
            .await?;
        let Response::BlockHashes(remote_hashes) = find_blocks else {
            panic!("unexpected FindBlocks response: {find_blocks:?}");
        };
        assert_eq!(remote_hashes, vec![block.hash()]);

        let block_response = adapter
            .request_from_source(
                Request::BlocksByHashFrom {
                    hashes: remote_hashes.into_iter().collect(),
                    source: PeerSource::Zakura(a_peer_id),
                },
                None,
            )
            .await?;
        let Response::Blocks(blocks) = block_response else {
            panic!("unexpected block response: {block_response:?}");
        };
        for response in blocks {
            let received = response
                .available()
                .expect("mock sync peer serves the advertised block")
                .0;
            local_chain.push(received.hash());
        }
        assert_eq!(local_chain.last(), Some(&block.hash()));
        assert_eq!(local_chain.len(), 2);

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn controlled_three_node_network_gossips_fetches_and_advances_mock_chain(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let transaction = UnminedTx::from(empty_v5_transaction(14));
        let (node_a, _pushed_rx) = normal_network_node(85, block.clone(), transaction).await?;
        let (node_b, mut rx_b) = legacy_node(86).await?;
        let (node_c, mut rx_c) = legacy_node(87).await?;

        node_a
            .connect_native(&node_b, Duration::from_secs(5))
            .await?;
        node_a
            .connect_native(&node_c, Duration::from_secs(5))
            .await?;
        node_b
            .connect_native(&node_c, Duration::from_secs(5))
            .await?;
        wait_registered_count(&node_a, 2).await?;
        wait_registered_count(&node_b, 2).await?;
        wait_registered_count(&node_c, 2).await?;

        let a_peer_id = node_peer_id(&node_a).await?;
        let mut gossip = LegacyGossipAdapter::new(node_a.supervisor());
        gossip
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(block.hash()))
            .await?;

        match recv_request(&mut rx_b).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(hash, block.hash());
                assert_eq!(peer_id, a_peer_id);
            }
            request => panic!("unexpected B request: {request:?}"),
        }
        match recv_request(&mut rx_c).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(_))) => {
                assert_eq!(hash, block.hash());
            }
            request => panic!("unexpected C request: {request:?}"),
        }

        let adapter = LegacyRequestAdapter::new(node_c.supervisor());
        let mut local_chain = vec![block_hash(1)];
        let headers = adapter
            .request_from_source(
                Request::FindHeaders {
                    known_blocks: local_chain.clone(),
                    stop: Some(block.hash()),
                },
                Some(PeerSource::Zakura(a_peer_id.clone())),
            )
            .await?;
        assert!(matches!(
            headers,
            Response::BlockHeaders(headers) if matches!(
                headers.as_slice(),
                [header] if header.header == block.header
            )
        ));

        let hashes = adapter
            .request_from_source(
                Request::FindBlocks {
                    known_blocks: local_chain.clone(),
                    stop: Some(block.hash()),
                },
                Some(PeerSource::Zakura(a_peer_id.clone())),
            )
            .await?;
        let Response::BlockHashes(hashes) = hashes else {
            panic!("unexpected FindBlocks response: {hashes:?}");
        };
        assert_eq!(hashes, vec![block.hash()]);

        let blocks = adapter
            .request_from_source(
                Request::BlocksByHashFrom {
                    hashes: hashes.into_iter().collect(),
                    source: PeerSource::Zakura(a_peer_id),
                },
                None,
            )
            .await?;
        let Response::Blocks(blocks) = blocks else {
            panic!("unexpected block response: {blocks:?}");
        };
        for response in blocks {
            let received = response
                .available()
                .expect("mock sync peer serves the gossiped block")
                .0;
            local_chain.push(received.hash());
        }
        assert_eq!(local_chain.last(), Some(&block.hash()));
        assert_eq!(local_chain.len(), 2);

        node_a.shutdown().await;
        node_b.shutdown().await;
        node_c.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_adapter_supports_mempool_ping_and_push_transaction() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let transaction = UnminedTx::from(empty_v5_transaction(5));
        let pushed_transaction = UnminedTx::from(empty_v5_transaction(6));
        let pushed_id = pushed_transaction.id;
        let (node_a, mut pushed_rx) = normal_network_node(83, block, transaction.clone()).await?;
        let node_b = ZakuraTestNode::builder(84).spawn().await?;
        node_b
            .connect_native(&node_a, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let adapter = LegacyRequestAdapter::new(node_b.supervisor());
        let mempool = adapter
            .request_from_source(
                Request::MempoolTransactionIds,
                Some(PeerSource::Zakura(a_peer_id.clone())),
            )
            .await?;
        assert_eq!(mempool, Response::TransactionIds(vec![transaction.id]));

        let ping = adapter
            .request_from_source(
                Request::Ping(Default::default()),
                Some(PeerSource::Zakura(a_peer_id)),
            )
            .await?;
        assert!(matches!(ping, Response::Pong(_)));

        let push_response = adapter
            .request_from_source(Request::PushTransaction(pushed_transaction), None)
            .await?;
        assert_eq!(push_response, Response::Nil);
        assert_eq!(recv_pushed_tx_id(&mut pushed_rx).await?, pushed_id);

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn slow_request_does_not_block_gossip_worker() -> Result<(), BoxError> {
        let (request_tx, request_rx) = oneshot::channel();
        let (gossip_tx, mut gossip_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let (inbound_tx, inbound_rx) = mpsc::channel(4);
        let supervisor = ZakuraSupervisorHandle::new(1);
        let peer_id = ZakuraPeerId::new(vec![6; 32]).expect("test peer id is within bounds");

        let worker = tokio::spawn(legacy_gossip_worker(
            SlowRequestThenRecorder {
                release: release_rx,
                tx: gossip_tx,
            },
            inbound_rx,
            LegacyGossipForwarder::new(supervisor),
            ZakuraTrace::noop(),
        ));

        inbound_tx
            .send(LegacyInboundWork::Request(LegacyRequestInbound {
                peer_id: peer_id.clone(),
                request_id: 1,
                frame: LegacyRequestFrame::BlocksByHash(vec![block_hash(11)]),
                response_tx: request_tx,
            }))
            .await?;
        inbound_tx
            .send(LegacyInboundWork::Gossip(LegacyGossipInbound {
                peer_id: peer_id.clone(),
                frame: LegacyGossipFrame::AdvertiseBlock(block_hash(12)),
            }))
            .await?;

        match recv_request(&mut gossip_rx).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(source))) => {
                assert_eq!(hash, block_hash(12));
                assert_eq!(source, peer_id);
            }
            request => panic!("unexpected request: {request:?}"),
        }

        release_tx.send(true)?;
        match tokio::time::timeout(Duration::from_secs(1), request_rx).await??? {
            Response::Blocks(blocks) => {
                assert_eq!(blocks, vec![InventoryResponse::Missing(block_hash(11))]);
            }
            response => panic!("unexpected request response: {response:?}"),
        }

        drop(inbound_tx);
        worker.await?;
        Ok(())
    }

    /// Regression: while the legacy inbound service is slow/timing out,
    /// `handle_legacy_gossip` returns without `mark_seen`, so an authenticated peer
    /// could replay the same valid advertisement and make the serial worker re-pay
    /// the full readiness/call timeout for every queued duplicate. After one
    /// expensive failed attempt the cooldown must suppress identical duplicates.
    ///
    /// Paused time auto-advances the `LEGACY_GOSSIP_SERVICE_TIMEOUT` so the first
    /// call's timeout fires without a real wall-clock wait.
    #[tokio::test(start_paused = true)]
    async fn duplicate_gossip_does_not_repeat_expensive_attempts_while_service_is_slow(
    ) -> Result<(), BoxError> {
        let calls = Arc::new(AtomicUsize::new(0));
        let (inbound_tx, inbound_rx) = mpsc::channel(16);
        let supervisor = ZakuraSupervisorHandle::new(1);
        let peer_id = ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds");

        let worker = tokio::spawn(legacy_gossip_worker(
            CountingPendingService {
                calls: calls.clone(),
            },
            inbound_rx,
            LegacyGossipForwarder::new(supervisor),
            ZakuraTrace::noop(),
        ));

        // The call never completes, so the permanent first-seen cache is never
        // updated; only the post-timeout cooldown can suppress these duplicates.
        let frame = LegacyGossipFrame::AdvertiseBlock(block_hash(99));
        for _ in 0..4 {
            inbound_tx
                .send(LegacyInboundWork::Gossip(LegacyGossipInbound {
                    peer_id: peer_id.clone(),
                    frame: frame.clone(),
                }))
                .await?;
        }

        drop(inbound_tx);
        worker.await?;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "duplicate gossip frames must not each re-pay the inbound readiness/call \
             timeout while the service is slow; expected one expensive attempt with \
             the rest suppressed by the cooldown"
        );

        Ok(())
    }

    #[tokio::test]
    async fn service_request_routes_to_advertiser_then_fallback_after_missing(
    ) -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(3));
        let txid = transaction.id;
        let (advertiser, mut advertiser_rx) = recording_inventory_node(65, None).await?;
        let (fallback, mut fallback_rx) =
            recording_inventory_node(66, Some(transaction.clone())).await?;
        // The requester dials two loopback peers (advertiser + fallback), so it
        // opts out of the production per-IP cap (1) that the default test node
        // now enforces; this test exercises service routing, not per-IP admission.
        let requester = ZakuraTestNode::builder(67)
            .max_connections_per_ip(8)
            .spawn()
            .await?;
        requester
            .connect_native(&advertiser, Duration::from_secs(5))
            .await?;
        requester
            .connect_native(&fallback, Duration::from_secs(5))
            .await?;
        wait_registered_count(&requester, 2).await?;
        let advertiser_id = node_peer_id(&advertiser).await?;

        let mut adapter = LegacyRequestAdapter::new(requester.supervisor());
        let response = adapter
            .ready()
            .await?
            .call(Request::TransactionsByIdFrom {
                ids: HashSet::from([txid]),
                source: PeerSource::Zakura(advertiser_id),
            })
            .await?;

        match recv_request(&mut advertiser_rx).await? {
            Request::TransactionsById(ids) => assert_eq!(ids, HashSet::from([txid])),
            request => panic!("unexpected advertiser request: {request:?}"),
        }
        match recv_request(&mut fallback_rx).await? {
            Request::TransactionsById(ids) => assert_eq!(ids, HashSet::from([txid])),
            request => panic!("unexpected fallback request: {request:?}"),
        }

        let Response::Transactions(transactions) = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(matches!(
            transactions.as_slice(),
            [InventoryResponse::Available((tx, None))] if tx.id == transaction.id
        ));

        advertiser.shutdown().await;
        fallback.shutdown().await;
        requester.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn first_seen_gossip_forwards_across_line_and_drops_duplicates() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let (node_a, mut rx_a) = legacy_node(21).await?;
        let (node_b, mut rx_b) = legacy_node(22).await?;
        let (node_c, mut rx_c) = legacy_node(23).await?;

        node_a
            .connect_native(&node_b, Duration::from_secs(5))
            .await?;
        node_b
            .connect_native(&node_c, Duration::from_secs(5))
            .await?;

        wait_registered_count(&node_b, 2).await?;

        let a_peer_id = node_peer_id(&node_a).await?;
        let b_peer_id = node_peer_id(&node_b).await?;
        let block_hash = block_hash(42);
        let mut adapter = LegacyGossipAdapter::new(node_a.supervisor());
        adapter
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(block_hash))
            .await?;

        match recv_request(&mut rx_b).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(hash, block_hash);
                assert_eq!(peer_id, a_peer_id);
            }
            request => panic!("unexpected B request: {request:?}"),
        }
        match recv_request(&mut rx_c).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(hash, block_hash);
                assert_eq!(peer_id, b_peer_id);
            }
            request => panic!("unexpected C request: {request:?}"),
        }
        assert!(rx_a.try_recv().is_err());

        adapter
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(block_hash))
            .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx_c.try_recv().is_err());

        node_a.shutdown().await;
        node_b.shutdown().await;
        node_c.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn local_origin_echo_is_dropped_by_shared_first_seen_cache() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let (node_a, mut rx_a) = legacy_node(51).await?;
        let (node_b, mut rx_b) = legacy_node(52).await?;
        node_a
            .connect_native(&node_b, Duration::from_secs(5))
            .await?;
        let hostile = HostilePeer::connect_native(&node_a, 53).await?;
        wait_registered_count(&node_a, 2).await?;

        let block_hash = block_hash(88);
        let mut adapter = LegacyGossipAdapter::new(node_a.supervisor());
        adapter
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(block_hash))
            .await?;
        let _ = recv_request(&mut rx_b).await?;

        let echo_payload = LegacyGossipFrame::AdvertiseBlock(block_hash)
            .encode_frame()?
            .payload;
        hostile
            .send_frame(ZAKURA_STREAM_GOSSIP, echo_payload)
            .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            rx_a.try_recv().is_err(),
            "origin must not accept its own echo"
        );
        assert!(
            rx_b.try_recv().is_err(),
            "origin must not re-forward its own echo"
        );

        hostile.shutdown().await;
        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn saturated_peer_does_not_block_honest_fanout() -> Result<(), BoxError> {
        let saturated_peer = ZakuraPeerId::new(vec![1; 32]).expect("test peer id is within bounds");
        let honest_peer = ZakuraPeerId::new(vec![2; 32]).expect("test peer id is within bounds");
        let (saturated, _saturated_rx) = framed_channel(1);
        saturated.try_send(Frame {
            message_type: MSG_ADVERTISE_BLOCK,
            flags: 0,
            payload: vec![1],
        })?;

        let (honest, mut honest_rx) = framed_channel(1);
        let block_hash = block_hash(7);

        let result = send_to_sessions(
            vec![
                LegacyGossipPeerSession::new(saturated_peer, saturated),
                LegacyGossipPeerSession::new(honest_peer, honest),
            ],
            LegacyGossipFrame::AdvertiseBlock(block_hash),
        );
        let outbound = tokio::time::timeout(Duration::from_secs(1), honest_rx.recv())
            .await?
            .expect("honest peer receives fanout");
        assert_eq!(
            LegacyGossipFrame::decode_frame(outbound)?,
            LegacyGossipFrame::AdvertiseBlock(block_hash)
        );

        assert!(
            result.is_err(),
            "saturated peer failure is reported after honest peer is attempted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn new_peer_receives_latest_block_advertised_before_gossip_stream_ready(
    ) -> Result<(), BoxError> {
        let supervisor = ZakuraSupervisorHandle::new(991);
        let broadcast = ZakuraGossipBroadcast::new(supervisor);
        let block_hash = block_hash(91);

        broadcast
            .broadcast(LegacyGossipFrame::AdvertiseBlock(block_hash), None)
            .await?;

        let peer_id = ZakuraPeerId::new(vec![91; 32]).expect("test peer id is within bounds");
        let (sender, mut receiver) = framed_channel(1);
        let session = LegacyGossipPeerSession::new(peer_id, sender);
        broadcast.outbound.insert(session.clone());
        broadcast
            .outbound
            .replay_latest_block_to_peer(session)
            .await?;

        let replayed = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await?
            .expect("new peer receives latest block replay");
        assert_eq!(
            LegacyGossipFrame::decode_frame(replayed)?,
            LegacyGossipFrame::AdvertiseBlock(block_hash)
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_gossip_replay_panic_cancels_peer_and_removes_outbound_session(
    ) -> Result<(), BoxError> {
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let outbound = LegacyGossipOutbound::default();
        let sink = LegacyGossipSink {
            inbound_tx,
            outbound: outbound.clone(),
            trace: ZakuraTrace::noop(),
        };
        let peer_id = ZakuraPeerId::new(vec![92; 32]).expect("test peer id is within bounds");
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let (peer, _peer_send) = legacy_gossip_peer(peer_id.clone(), cancel_token.clone());

        let poisoned_latest_block = outbound.latest_block.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned_latest_block
                .lock()
                .expect("latest-block mutex starts unpoisoned");
            panic!("poison latest-block mutex before replay");
        });

        sink.add_peer(peer);
        wait_for_legacy_gossip_panic_cleanup(&outbound, &peer_id, &cancel_token).await
    }

    #[tokio::test]
    async fn legacy_gossip_recv_loop_panic_cancels_peer_and_removes_outbound_session(
    ) -> Result<(), BoxError> {
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let outbound = LegacyGossipOutbound::default();
        let sink = LegacyGossipSink {
            inbound_tx,
            outbound: outbound.clone(),
            trace: ZakuraTrace::noop(),
        };
        let peer_id = ZakuraPeerId::new(vec![93; 32]).expect("test peer id is within bounds");
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let (peer, peer_send) = legacy_gossip_peer(peer_id.clone(), cancel_token.clone());

        arm_legacy_gossip_recv_loop_panic(peer_id.clone());
        sink.add_peer(peer);
        peer_send
            .send(LegacyGossipFrame::AdvertiseBlock(block_hash(93)).encode_frame()?)
            .await
            .map_err(|_| -> BoxError { "failed to send test gossip frame".into() })?;

        wait_for_legacy_gossip_panic_cleanup(&outbound, &peer_id, &cancel_token).await
    }

    #[tokio::test]
    async fn disconnected_peer_send_returns_error() -> Result<(), BoxError> {
        let peer_id = ZakuraPeerId::new(vec![9; 32]).expect("test peer id is within bounds");
        let (disconnected, rx) = framed_channel(1);
        drop(rx);

        let error = send_to_sessions(
            vec![LegacyGossipPeerSession::new(peer_id, disconnected)],
            LegacyGossipFrame::AdvertiseBlock(block_hash(8)),
        )
        .expect_err("closed outbound queue reports an adapter error");
        assert!(
            error
                .to_string()
                .contains("ordered stream send queue is closed"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn transient_inbound_failure_does_not_poison_first_seen_cache() -> Result<(), BoxError> {
        let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let (inbound_tx, inbound_rx) = mpsc::channel(4);
        let supervisor = ZakuraSupervisorHandle::new(1);
        let peer_id = ZakuraPeerId::new(vec![3; 32]).expect("test peer id is within bounds");
        let frame = LegacyGossipFrame::AdvertiseBlock(block_hash(5));

        let worker = tokio::spawn(legacy_gossip_worker(
            FailsOnceRecorder {
                attempts: attempts.clone(),
                tx: request_tx,
            },
            inbound_rx,
            LegacyGossipForwarder::new(supervisor),
            ZakuraTrace::noop(),
        ));

        inbound_tx
            .send(LegacyInboundWork::Gossip(LegacyGossipInbound {
                peer_id: peer_id.clone(),
                frame: frame.clone(),
            }))
            .await?;
        inbound_tx
            .send(LegacyInboundWork::Gossip(LegacyGossipInbound {
                peer_id: peer_id.clone(),
                frame: frame.clone(),
            }))
            .await?;

        match recv_request(&mut request_rx).await? {
            Request::AdvertiseBlock(hash, Some(PeerSource::Zakura(source))) => {
                assert_eq!(hash, block_hash(5));
                assert_eq!(source, peer_id);
            }
            request => panic!("unexpected request: {request:?}"),
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        inbound_tx
            .send(LegacyInboundWork::Gossip(LegacyGossipInbound {
                peer_id,
                frame,
            }))
            .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "successful delivery marks the inventory seen"
        );

        drop(inbound_tx);
        worker.await?;
        Ok(())
    }

    #[test]
    fn inbound_queue_full_drops_without_rejecting_peer_but_malformed_rejects() {
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let sink = LegacyGossipSink {
            inbound_tx,
            outbound: LegacyGossipOutbound::default(),
            trace: ZakuraTrace::noop(),
        };
        let peer_id = ZakuraPeerId::new(vec![4; 32]).expect("test peer id is within bounds");
        let frame = LegacyGossipFrame::AdvertiseBlock(block_hash(6))
            .encode_frame()
            .expect("frame encodes");

        assert!(sink
            .deliver(peer_id.clone(), ZAKURA_STREAM_GOSSIP, frame.clone())
            .is_ok());
        assert!(
            sink.deliver(peer_id.clone(), ZAKURA_STREAM_GOSSIP, frame)
                .is_ok(),
            "queue-full overload is shed without disconnecting the peer"
        );
        assert!(sink
            .deliver(
                peer_id,
                ZAKURA_STREAM_GOSSIP,
                Frame {
                    message_type: MSG_ADVERTISE_BLOCK,
                    flags: 1,
                    payload: Vec::new(),
                },
            )
            .is_err());
    }

    #[test]
    fn tx_gossip_round_trips_legacy_and_witnessed_ids() {
        let frame =
            LegacyGossipFrame::AdvertiseTransactionIds(vec![legacy_tx_id(2), witnessed_tx_id(3)]);

        assert_eq!(
            LegacyGossipFrame::decode_frame(frame.encode_frame().expect("frame encodes"))
                .expect("frame decodes"),
            frame
        );
    }

    #[test]
    fn inventory_request_frames_round_trip_and_enforce_bounds() {
        let block_request = LegacyRequestFrame::BlocksByHash(vec![block_hash(1), block_hash(2)]);
        assert_eq!(
            LegacyRequestFrame::decode_frame(block_request.encode_frame().expect("frame encodes"))
                .expect("frame decodes"),
            block_request
        );

        let tx_request =
            LegacyRequestFrame::TransactionsById(vec![legacy_tx_id(3), witnessed_tx_id(4)]);
        assert_eq!(
            LegacyRequestFrame::decode_frame(tx_request.encode_frame().expect("frame encodes"))
                .expect("frame decodes"),
            tx_request
        );

        let find_blocks = LegacyRequestFrame::FindBlocks {
            known_blocks: vec![block_hash(5)],
            stop: Some(block_hash(6)),
        };
        assert_eq!(
            LegacyRequestFrame::decode_frame(find_blocks.encode_frame().expect("frame encodes"))
                .expect("frame decodes"),
            find_blocks
        );

        let find_headers = LegacyRequestFrame::FindHeaders {
            known_blocks: vec![block_hash(7)],
            stop: None,
        };
        assert_eq!(
            LegacyRequestFrame::decode_frame(find_headers.encode_frame().expect("frame encodes"))
                .expect("frame decodes"),
            find_headers
        );

        for request in [
            LegacyRequestFrame::MempoolTransactionIds,
            LegacyRequestFrame::Ping,
            LegacyRequestFrame::PushTransaction(UnminedTx::from(empty_v5_transaction(8))),
        ] {
            assert_eq!(
                LegacyRequestFrame::decode_frame(request.encode_frame().expect("frame encodes"))
                    .expect("frame decodes"),
                request
            );
        }

        let max = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE).expect("test cap fits usize");
        let oversized = Frame {
            message_type: MSG_REQUEST_BLOCKS_BY_HASH,
            flags: 0,
            payload: encoded_count(max + 1),
        };
        assert!(matches!(
            LegacyRequestFrame::decode_frame(oversized),
            Err(LegacyGossipError::TooManyInventoryItems(count)) if count == max + 1
        ));

        let max_locator =
            usize::try_from(MAX_BLOCK_LOCATOR_LENGTH).expect("test locator cap fits usize");
        let oversized_locator = Frame {
            message_type: MSG_REQUEST_FIND_HEADERS,
            flags: 0,
            payload: encoded_count(max_locator + 1),
        };
        assert!(matches!(
            LegacyRequestFrame::decode_frame(oversized_locator),
            Err(LegacyGossipError::TooManyBlockLocatorHashes(count)) if count == max_locator + 1
        ));
    }

    #[test]
    fn response_codec_rejects_wrong_request_id_and_oversized_chunks() {
        let mut wrong_id_payload = Vec::new();
        wrong_id_payload.extend_from_slice(&2_u64.to_le_bytes());
        wrong_id_payload.push(1);
        let wrong_id = Frame {
            message_type: MSG_RESPONSE_BLOCK,
            flags: 0,
            payload: wrong_id_payload,
        };
        assert!(matches!(
            LegacyResponseCodec::decode_response(
                1,
                LegacyRequestKind::Blocks,
                vec![wrong_id],
                None
            ),
            Err(LegacyGossipError::WrongRequestId {
                expected: 1,
                actual: 2
            })
        ));

        let mut oversized_payload = Vec::new();
        oversized_payload.extend_from_slice(&1_u64.to_le_bytes());
        oversized_payload.push(0);
        oversized_payload.resize(
            MAX_PROTOCOL_MESSAGE_LEN + RESPONSE_CHUNK_HEADER_BYTES + 1,
            0,
        );
        let oversized = Frame {
            message_type: MSG_RESPONSE_BLOCK,
            flags: 0,
            payload: oversized_payload,
        };
        assert!(matches!(
            LegacyResponseCodec::decode_response(
                1,
                LegacyRequestKind::Blocks,
                vec![oversized],
                None
            ),
            Err(LegacyGossipError::OversizedResponse(_))
        ));
    }

    /// The inbound service returns `Response::Nil` (a lone nil frame) for an empty
    /// FindBlocks/FindHeaders/MempoolTransactionIds result, so the codec must
    /// decode that into each kind's empty response rather than rejecting it.
    #[test]
    fn nil_response_decodes_as_each_kinds_empty_result() {
        let request_id = 7;
        let nil = || id_only_frame(MSG_RESPONSE_NIL, request_id);

        assert_eq!(
            LegacyResponseCodec::decode_response(
                request_id,
                LegacyRequestKind::FindBlocks,
                vec![nil()],
                None,
            )
            .expect("nil is a valid empty FindBlocks response"),
            Response::BlockHashes(vec![]),
        );
        assert_eq!(
            LegacyResponseCodec::decode_response(
                request_id,
                LegacyRequestKind::FindHeaders,
                vec![nil()],
                None,
            )
            .expect("nil is a valid empty FindHeaders response"),
            Response::BlockHeaders(vec![]),
        );
        assert_eq!(
            LegacyResponseCodec::decode_response(
                request_id,
                LegacyRequestKind::MempoolTransactionIds,
                vec![nil()],
                None,
            )
            .expect("nil is a valid empty mempool response"),
            Response::TransactionIds(vec![]),
        );
        assert_eq!(
            LegacyResponseCodec::decode_response(
                request_id,
                LegacyRequestKind::PushTransaction,
                vec![nil()],
                None,
            )
            .expect("nil acknowledges a pushed transaction"),
            Response::Nil,
        );

        // Inventory fetches and Ping must NOT accept a bare nil: for those it
        // means the peer has nothing, so the codec rejects it and the caller
        // falls back to another peer instead of returning an empty fetch.
        for kind in [
            LegacyRequestKind::Blocks,
            LegacyRequestKind::Transactions,
            LegacyRequestKind::Ping,
        ] {
            assert!(
                matches!(
                    LegacyResponseCodec::decode_response(request_id, kind, vec![nil()], None),
                    Err(LegacyGossipError::UnexpectedResponse(_)),
                ),
                "nil must be rejected for {kind:?} so the caller can fall back",
            );
        }
    }

    #[test]
    fn response_codec_chunks_to_negotiated_frame_limit() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let max_frame_bytes =
            u32::try_from(FRAME_HEADER_BYTES + RESPONSE_CHUNK_HEADER_BYTES + 256)?;
        let frames = LegacyResponseCodec::encode_response(
            7,
            Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))]),
            max_frame_bytes,
            max_frame_bytes,
        )?;

        assert!(frames.len() > 1, "large block response must be chunked");
        for frame in &frames {
            frame.encode(max_frame_bytes)?;
        }

        let response =
            LegacyResponseCodec::decode_response(7, LegacyRequestKind::Blocks, frames, None)?;
        let Response::Blocks(blocks) = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(matches!(
            blocks.as_slice(),
            [InventoryResponse::Available((received, None))] if received.hash() == block.hash()
        ));

        Ok(())
    }

    /// Regression test for `claude-outbound-write-ignores-message-cap` (legacy
    /// response chunking facet).
    ///
    /// The handshake clamps `max_frame_bytes` and `max_message_bytes`
    /// independently, so a peer can negotiate a message cap well below the frame
    /// cap. `push_chunked_response` sized chunk frames against the frame cap
    /// alone, so each chunk frame's payload could exceed the peer's accepted
    /// `max_message_bytes`. The request-response writer (`write_response_frame`)
    /// and the peer both reject such a frame as oversize, wasting the encode and
    /// causing avoidable disconnects/interop loss. Chunking must size against the
    /// effective cap `min(frame cap, message cap)` so every emitted chunk frame
    /// fits the negotiated message cap while still round-tripping.
    #[test]
    fn encode_response_chunks_respect_message_cap() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);

        // Large frame cap, small message cap: the divergence the handshake
        // permits. The message cap allows a 256-byte chunk payload plus the
        // per-chunk response header.
        let max_frame_bytes = u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?;
        let max_message_bytes = u32::try_from(RESPONSE_CHUNK_HEADER_BYTES + 256)?;

        let frames = LegacyResponseCodec::encode_response(
            7,
            Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))]),
            max_frame_bytes,
            max_message_bytes,
        )?;

        assert!(
            frames.len() > 1,
            "a block larger than the negotiated message cap must be chunked, not emitted as one \
             over-cap frame"
        );
        for frame in &frames {
            assert!(
                frame.payload.len() <= max_message_bytes as usize,
                "chunk frame payload {} exceeds the negotiated max_message_bytes {}; the peer \
                 (and write_response_frame) would reject it as oversize",
                frame.payload.len(),
                max_message_bytes,
            );
            // Each chunk must still fit the frame cap so the transport encodes it.
            frame.encode(max_frame_bytes)?;
        }

        // The smaller chunking must still round-trip back to the original block.
        let response =
            LegacyResponseCodec::decode_response(7, LegacyRequestKind::Blocks, frames, None)?;
        let Response::Blocks(blocks) = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(matches!(
            blocks.as_slice(),
            [InventoryResponse::Available((received, None))] if received.hash() == block.hash()
        ));

        Ok(())
    }

    /// A legacy block response must be bound to a hash we actually requested.
    ///
    /// Block responses are correlated only by request id and request kind, so
    /// without binding, a peer can substitute any other valid block for the one
    /// requested. The decoder must reject a delivered block whose hash is not in
    /// the requested set, and accept it when it is.
    #[test]
    fn decode_response_binds_blocks_to_requested_hashes() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let frame_cap = u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?;
        let frames = LegacyResponseCodec::encode_response(
            7,
            Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))]),
            frame_cap,
            frame_cap,
        )?;

        // A peer substitutes a real block we never asked for. Bound against a
        // requested-hash set that does not contain it, the codec rejects it
        // instead of correlating the response by request id and kind alone.
        let unrelated: HashSet<block::Hash> = std::iter::once(block_hash(99)).collect();
        assert!(
            matches!(
                LegacyResponseCodec::decode_response(
                    7,
                    LegacyRequestKind::Blocks,
                    frames.clone(),
                    Some(&unrelated),
                ),
                Err(LegacyGossipError::UnsolicitedBlock(_)),
            ),
            "a block whose hash was not requested must be rejected",
        );

        // The same response is accepted when its hash is among those requested.
        let requested: HashSet<block::Hash> = std::iter::once(block.hash()).collect();
        let response = LegacyResponseCodec::decode_response(
            7,
            LegacyRequestKind::Blocks,
            frames,
            Some(&requested),
        )?;
        assert!(matches!(
            response,
            Response::Blocks(blocks)
                if matches!(
                    blocks.as_slice(),
                    [InventoryResponse::Available((received, None))]
                        if received.hash() == block.hash()
                )
        ));

        Ok(())
    }

    /// Regression test for `claude-legacy-responder-response-aggregation-unbounded`.
    ///
    /// An authenticated peer can name up to `MAX_TX_INV_IN_SENT_MESSAGE`
    /// block/transaction hashes on one request. Without a responder-side
    /// aggregate budget, `encode_response` serializes and retains the entire
    /// multi-frame `Vec<Frame>` for every available item before the first byte
    /// is written (worst case `MAX_TX_INV_IN_SENT_MESSAGE *
    /// MAX_PROTOCOL_MESSAGE_LEN`, tens of GiB). The outbound reader already
    /// enforces a symmetric `LegacyResponseBudget`; the responder must too, and
    /// must abort encoding early rather than buffering an over-budget response.
    #[test]
    fn encode_response_aborts_when_aggregation_exceeds_budget() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let block_bytes = block.zcash_serialize_to_vec()?.len();
        let frame_cap = u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?;

        // A single available block is well within budget and still encodes.
        LegacyResponseCodec::encode_response(
            1,
            Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))]),
            frame_cap,
            frame_cap,
        )?;

        // Enough available blocks to overflow the cumulative byte budget must be
        // rejected, not fully materialized as a `Vec<Frame>`.
        let block_copies = LEGACY_RESPONSE_MAX_AGGREGATE_BYTES / block_bytes + 2;
        let many_blocks = Response::Blocks(
            (0..block_copies)
                .map(|_| InventoryResponse::Available((block.clone(), None)))
                .collect(),
        );
        assert!(
            matches!(
                LegacyResponseCodec::encode_response(2, many_blocks, frame_cap, frame_cap),
                Err(LegacyGossipError::ResponseAggregateBudget(_)),
            ),
            "an over-budget BlocksByHash response must abort encoding early",
        );

        // The same cumulative budget guards the transaction responder path,
        // which shares `push_chunked_response`.
        let tx_copies = 2 * LEGACY_RESPONSE_MAX_AGGREGATE_BYTES / block_bytes + 2;
        let many_transactions = Response::Transactions(
            (0..tx_copies)
                .flat_map(|_| {
                    block
                        .transactions
                        .iter()
                        .map(|tx| InventoryResponse::Available((UnminedTx::from(tx.clone()), None)))
                })
                .collect(),
        );
        assert!(
            matches!(
                LegacyResponseCodec::encode_response(3, many_transactions, frame_cap, frame_cap),
                Err(LegacyGossipError::ResponseAggregateBudget(_)),
            ),
            "an over-budget TransactionsById response must abort encoding early",
        );

        Ok(())
    }

    #[test]
    fn response_codec_round_trips_chain_sync_and_mempool_responses() -> Result<(), BoxError> {
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let header = block::CountedHeader {
            header: block.header.clone(),
        };

        let block_hash_response = Response::BlockHashes(vec![block.hash(), block_hash(10)]);
        let frames = LegacyResponseCodec::encode_response(
            8,
            block_hash_response.clone(),
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
        )?;
        assert_eq!(
            LegacyResponseCodec::decode_response(8, LegacyRequestKind::FindBlocks, frames, None)?,
            block_hash_response
        );

        let header_response = Response::BlockHeaders(vec![header]);
        let frames = LegacyResponseCodec::encode_response(
            9,
            header_response.clone(),
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
        )?;
        assert_eq!(
            LegacyResponseCodec::decode_response(9, LegacyRequestKind::FindHeaders, frames, None)?,
            header_response
        );

        let tx_ids_response = Response::TransactionIds(vec![legacy_tx_id(11), witnessed_tx_id(12)]);
        let frames = LegacyResponseCodec::encode_response(
            10,
            tx_ids_response.clone(),
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
        )?;
        assert_eq!(
            LegacyResponseCodec::decode_response(
                10,
                LegacyRequestKind::MempoolTransactionIds,
                frames,
                None,
            )?,
            tx_ids_response
        );

        let frames = LegacyResponseCodec::encode_response(
            11,
            Response::Pong(Duration::ZERO),
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
        )?;
        assert!(matches!(
            LegacyResponseCodec::decode_response(11, LegacyRequestKind::Ping, frames, None)?,
            Response::Pong(_)
        ));

        let frames = LegacyResponseCodec::encode_response(
            12,
            Response::Nil,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
            u32::try_from(MAX_PROTOCOL_MESSAGE_LEN)?,
        )?;
        assert_eq!(
            LegacyResponseCodec::decode_response(
                12,
                LegacyRequestKind::PushTransaction,
                frames,
                None
            )?,
            Response::Nil
        );

        Ok(())
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(matches!(
            LegacyGossipFrame::decode_frame(Frame {
                message_type: MSG_ADVERTISE_BLOCK,
                flags: 1,
                payload: Vec::new(),
            }),
            Err(LegacyGossipError::UnsupportedFlags(1))
        ));
        assert!(matches!(
            LegacyGossipFrame::decode_frame(Frame {
                message_type: 99,
                flags: 0,
                payload: Vec::new(),
            }),
            Err(LegacyGossipError::UnknownMessageType(99))
        ));
        assert!(matches!(
            LegacyGossipFrame::decode_frame(Frame {
                message_type: MSG_ADVERTISE_TX_IDS,
                flags: 0,
                payload: vec![0],
            }),
            Err(LegacyGossipError::EmptyTransactionAdvertisement)
        ));
    }

    #[test]
    fn tx_id_limits_are_enforced_before_allocation() {
        let max = usize::try_from(MAX_TX_INV_IN_SENT_MESSAGE).expect("test cap fits usize");
        let oversized = Frame {
            message_type: MSG_ADVERTISE_TX_IDS,
            flags: 0,
            payload: encoded_count(max + 1),
        };
        assert!(matches!(
            LegacyGossipFrame::decode_frame(oversized),
            Err(LegacyGossipError::TooManyInventoryItems(count)) if count == max + 1
        ));

        let ids: HashSet<_> = (0..max + 3)
            .map(|index| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(
                    &u64::try_from(index)
                        .expect("test index fits u64")
                        .to_le_bytes(),
                );
                UnminedTxId::from_legacy_id(transaction::Hash(bytes))
            })
            .collect();
        let frame = LegacyGossipFrame::from_request(Request::AdvertiseTransactionIds(ids, None))
            .expect("non-empty tx gossip converts");
        let LegacyGossipFrame::AdvertiseTransactionIds(ids) = frame else {
            panic!("expected tx id frame");
        };
        assert!(ids.len() <= max);
    }

    #[test]
    fn unsupported_requests_fail_loudly() {
        let unsupported = [
            (
                Request::BlocksByHash(HashSet::from([block_hash(1)])),
                "BlocksByHash",
            ),
            (
                Request::TransactionsById(HashSet::from([legacy_tx_id(1)])),
                "TransactionsById",
            ),
            (
                Request::FindBlocks {
                    known_blocks: vec![block_hash(2)],
                    stop: None,
                },
                "FindBlocks",
            ),
            (
                Request::FindHeaders {
                    known_blocks: vec![block_hash(3)],
                    stop: None,
                },
                "FindHeaders",
            ),
            (
                Request::PushTransaction(
                    Transaction::V5 {
                        network_upgrade: NetworkUpgrade::Nu5,
                        lock_time: LockTime::min_lock_time_timestamp(),
                        expiry_height: block::Height(0),
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        sapling_shielded_data: None,
                        orchard_shielded_data: None,
                    }
                    .into(),
                ),
                "PushTransaction",
            ),
            (Request::MempoolTransactionIds, "MempoolTransactionIds"),
            (Request::Peers, "Peers"),
        ];

        for (request, command) in unsupported {
            let error =
                LegacyGossipFrame::from_request(request).expect_err("request is unsupported");
            assert!(matches!(
                error,
                LegacyGossipError::UnsupportedRequest(unsupported) if unsupported == command
            ));
        }
    }

    #[test]
    fn legacy_peers_request_is_deferred_to_native_bootstrap_configuration() {
        let error =
            LegacyRequestFrame::from_request(Request::Peers).expect_err("Peers is deferred");
        assert!(matches!(
            error,
            LegacyGossipError::UnsupportedRequest("Peers")
        ));
        assert!(
            error
                .to_string()
                .contains("unsupported legacy gossip request"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn first_seen_cache_is_bounded_and_expires() {
        let cache = FirstSeenCache::new(1, Duration::from_millis(20));
        assert_eq!(
            cache
                .record_unseen([InventoryKey::Block(block_hash(1))])
                .await,
            vec![InventoryKey::Block(block_hash(1))]
        );
        assert!(cache
            .record_unseen([InventoryKey::Block(block_hash(1))])
            .await
            .is_empty());
        assert_eq!(
            cache
                .record_unseen([InventoryKey::Block(block_hash(2))])
                .await,
            vec![InventoryKey::Block(block_hash(2))]
        );
        assert_eq!(
            cache
                .record_unseen([InventoryKey::Block(block_hash(1))])
                .await,
            vec![InventoryKey::Block(block_hash(1))]
        );

        let cache = FirstSeenCache::new(8, Duration::from_millis(1));
        assert_eq!(
            cache
                .record_unseen([InventoryKey::Block(block_hash(3))])
                .await,
            vec![InventoryKey::Block(block_hash(3))]
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            cache
                .record_unseen([InventoryKey::Block(block_hash(3))])
                .await,
            vec![InventoryKey::Block(block_hash(3))]
        );
    }

    #[tokio::test]
    async fn malformed_inbound_gossip_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let (node, _rx) = legacy_node(31).await?;
        let hostile =
            HostilePeer::connect_native_with_capabilities(&node, 32, ZAKURA_CAP_LEGACY_GOSSIP)
                .await?;
        wait_registered_count(&node, 1).await?;

        hostile.send_frame(ZAKURA_STREAM_GOSSIP, vec![1]).await?;
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn gossip_stream_with_request_id_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(41).spawn().await?;
        let hostile = HostilePeer::connect_native(&node, 42).await?;
        wait_registered_count(&node, 1).await?;

        let frame = LegacyGossipFrame::AdvertiseBlock(block_hash(77)).encode_frame()?;
        hostile
            .send_frame_with_request_id(ZAKURA_STREAM_GOSSIP, 99, frame)
            .await?;
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_stream_without_request_id_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(71).spawn().await?;
        let hostile = HostilePeer::connect_native(&node, 72).await?;
        wait_registered_count(&node, 1).await?;

        let frame = LegacyRequestFrame::BlocksByHash(vec![block_hash(1)]).encode_frame()?;
        hostile
            .send_frame(ZAKURA_STREAM_LEGACY_REQUESTS, frame.payload)
            .await?;
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_outbound_response_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(73).spawn().await?;
        let hostile = HostilePeer::connect_native(&node, 74).await?;
        wait_registered_count(&node, 1).await?;
        let hostile_id = hostile.id()?;

        let mut wrong_id_payload = Vec::new();
        wrong_id_payload.extend_from_slice(&0_u64.to_le_bytes());
        wrong_id_payload.push(1);
        let response = Frame {
            message_type: MSG_RESPONSE_BLOCK,
            flags: 0,
            payload: wrong_id_payload,
        };

        let adapter = LegacyRequestAdapter::new(node.supervisor());
        let request = adapter.request_from_source(
            Request::BlocksByHash(HashSet::from([block_hash(1)])),
            Some(PeerSource::Zakura(hostile_id)),
        );
        let responder = hostile.respond_to_next_request(vec![response]);
        let (request_result, responder_result) = futures::join!(request, responder);

        responder_result?;
        let error = request_result.expect_err("wrong request id is a request error");
        assert!(
            error
                .to_string()
                .contains("wrong legacy response request id"),
            "unexpected error: {error}"
        );
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn outbound_request_timeout_releases_peer_for_next_request() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(79).spawn().await?;
        let mut hostile = HostilePeer::connect_native(&node, 80).await?;
        wait_registered_count(&node, 1).await?;
        let hostile_id = hostile.id()?;

        let adapter =
            LegacyRequestAdapter::new_with_timeout(node.supervisor(), Duration::from_millis(100));
        let first_request = adapter.request_from_source(
            Request::BlocksByHash(HashSet::from([block_hash(1)])),
            Some(PeerSource::Zakura(hostile_id.clone())),
        );
        let hold_first = hostile.accept_next_request_without_response();
        let (request_result, hold_result) = futures::join!(first_request, hold_first);

        hold_result?;
        let error = request_result.expect_err("silent peer should time out the request");
        assert!(
            error.to_string().contains("timed out"),
            "unexpected error: {error}"
        );
        wait_registered_count(&node, 1).await?;

        let second_hash = block_hash(2);
        let second_request = adapter.request_from_source(
            Request::BlocksByHash(HashSet::from([second_hash])),
            Some(PeerSource::Zakura(hostile_id)),
        );
        let second_response = hostile.respond_to_next_request_with(|request_id| {
            vec![missing_blocks_frame(request_id, vec![second_hash])
                .expect("one missing block hash fits in a response frame")]
        });
        let (request_result, response_result) = futures::join!(second_request, second_response);

        response_result?;
        assert_eq!(
            request_result?,
            Response::Blocks(vec![InventoryResponse::Missing(second_hash)])
        );

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn excessive_outbound_response_frames_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(75).spawn().await?;
        let hostile = HostilePeer::connect_native(&node, 76).await?;
        wait_registered_count(&node, 1).await?;
        let hostile_id = hostile.id()?;

        let adapter = LegacyRequestAdapter::new(node.supervisor());
        let request = adapter.request_from_source(
            Request::BlocksByHash(HashSet::from([block_hash(1)])),
            Some(PeerSource::Zakura(hostile_id)),
        );
        let responder = hostile.respond_to_next_request_with(|request_id| {
            let mut frames = Vec::new();
            for _ in 0..10 {
                let mut payload = Vec::new();
                payload.extend_from_slice(&request_id.to_le_bytes());
                payload.extend_from_slice(&encoded_count(0));
                frames.push(Frame {
                    message_type: MSG_RESPONSE_MISSING_BLOCKS,
                    flags: 0,
                    payload,
                });
            }
            frames
        });
        let (request_result, responder_result) = futures::join!(request, responder);

        responder_result?;
        let error = request_result.expect_err("excessive response frames are rejected");
        assert!(
            error
                .to_string()
                .contains("too many legacy response frames"),
            "unexpected error: {error}"
        );
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_outbound_block_response_disconnects_peer() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(77).spawn().await?;
        let hostile = HostilePeer::connect_native(&node, 78).await?;
        wait_registered_count(&node, 1).await?;
        let hostile_id = hostile.id()?;

        let adapter = LegacyRequestAdapter::new(node.supervisor());
        let request = adapter.request_from_source(
            Request::BlocksByHash(HashSet::from([block_hash(1)])),
            Some(PeerSource::Zakura(hostile_id)),
        );
        let responder = hostile.respond_to_next_request_with(|request_id| {
            let mut payload = Vec::new();
            payload.extend_from_slice(&request_id.to_le_bytes());
            payload.push(1);
            payload.extend_from_slice(b"not a serialized block");
            vec![Frame {
                message_type: MSG_RESPONSE_BLOCK,
                flags: 0,
                payload,
            }]
        });
        let (request_result, responder_result) = futures::join!(request, responder);

        responder_result?;
        request_result.expect_err("invalid block bytes are rejected");
        wait_registered_count(&node, 0).await?;

        hostile.shutdown().await;
        node.shutdown().await;
        Ok(())
    }

    /// How the legacy stub answers inventory fetches in dual-stack tests.
    #[derive(Clone)]
    enum StubInventory {
        /// Return every requested id as missing (triggers Zakura fallback).
        Missing,
        /// Return the given transaction as available.
        Available(UnminedTx),
        /// Error on inventory (used to prove the legacy path is bypassed).
        Error,
    }

    /// Stand-in for the legacy peer set inside a [`ZakuraDualStackService`]. It
    /// records every request it is handed and answers inventory per `inventory`.
    #[derive(Clone)]
    struct DualStackLegacyStub {
        tx: tokio::sync::mpsc::UnboundedSender<Request>,
        inventory: StubInventory,
    }

    impl Service<Request> for DualStackLegacyStub {
        type Response = Response;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request) -> Self::Future {
            let _ = self.tx.send(request.clone());
            let response = match request {
                Request::TransactionsById(ids) | Request::TransactionsByIdFrom { ids, .. } => {
                    match &self.inventory {
                        StubInventory::Error => {
                            return std::future::ready(Err("legacy inventory unavailable".into()))
                        }
                        StubInventory::Missing => Response::Transactions(
                            ids.into_iter().map(InventoryResponse::Missing).collect(),
                        ),
                        StubInventory::Available(tx) => Response::Transactions(
                            ids.into_iter()
                                .map(|id| {
                                    if id == tx.id {
                                        InventoryResponse::Available((tx.clone(), None))
                                    } else {
                                        InventoryResponse::Missing(id)
                                    }
                                })
                                .collect(),
                        ),
                    }
                }
                Request::BlocksByHash(hashes) | Request::BlocksByHashFrom { hashes, .. } => {
                    if let StubInventory::Error = self.inventory {
                        return std::future::ready(Err("legacy inventory unavailable".into()));
                    }
                    Response::Blocks(hashes.into_iter().map(InventoryResponse::Missing).collect())
                }
                _ => Response::Nil,
            };
            std::future::ready(Ok(response))
        }
    }

    #[tokio::test]
    async fn dual_stack_advertise_fans_out_to_legacy_and_zakura() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        // node_a originates; node_b records gossip delivered over Zakura.
        let (node_a, _rx_a) = legacy_node(201).await?;
        let (node_b, mut rx_b) = legacy_node(202).await?;
        node_a
            .connect_native(&node_b, Duration::from_secs(5))
            .await?;
        let a_peer_id = node_peer_id(&node_a).await?;

        let (legacy_tx, mut rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Missing,
            },
            node_a.supervisor(),
            true,
        );

        let hash = block_hash(9);
        composite
            .ready()
            .await?
            .call(Request::AdvertiseBlockToAll(hash))
            .await?;

        // Legacy peer set received the advertisement verbatim.
        match recv_request(&mut rx_legacy).await? {
            Request::AdvertiseBlockToAll(received) => assert_eq!(received, hash),
            request => panic!("unexpected legacy request: {request:?}"),
        }
        // Zakura delivered it to node_b, attributed to node_a.
        match recv_request(&mut rx_b).await? {
            Request::AdvertiseBlock(received, Some(PeerSource::Zakura(peer_id))) => {
                assert_eq!(received, hash);
                assert_eq!(peer_id, a_peer_id);
            }
            request => panic!("unexpected zakura request: {request:?}"),
        }

        node_a.shutdown().await;
        node_b.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn dual_stack_inventory_falls_back_to_zakura_when_legacy_missing() -> Result<(), BoxError>
    {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(7));
        let advertiser = inventory_node(203, transaction.clone()).await?;
        let requester = ZakuraTestNode::builder(204).spawn().await?;
        requester
            .connect_native(&advertiser, Duration::from_secs(5))
            .await?;

        let (legacy_tx, _rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Missing,
            },
            requester.supervisor(),
            true,
        );

        let response = composite
            .ready()
            .await?
            .call(Request::TransactionsById(HashSet::from([transaction.id])))
            .await?;
        match response {
            Response::Transactions(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], InventoryResponse::Available(_)));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        advertiser.shutdown().await;
        requester.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn dual_stack_inventory_uses_legacy_when_available() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(8));
        // No Zakura advertiser is connected, so any fallback would fail; the
        // legacy-available short-circuit is what makes this succeed.
        let requester = ZakuraTestNode::builder(205).spawn().await?;

        let (legacy_tx, _rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Available(transaction.clone()),
            },
            requester.supervisor(),
            true,
        );

        let response = composite
            .ready()
            .await?
            .call(Request::TransactionsById(HashSet::from([transaction.id])))
            .await?;
        match response {
            Response::Transactions(items) => {
                assert!(matches!(items[0], InventoryResponse::Available(_)));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        requester.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn dual_stack_zakura_only_bypasses_legacy_for_inventory() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let transaction = UnminedTx::from(empty_v5_transaction(9));
        let advertiser = inventory_node(206, transaction.clone()).await?;
        let requester = ZakuraTestNode::builder(207).spawn().await?;
        requester
            .connect_native(&advertiser, Duration::from_secs(5))
            .await?;

        // legacy_enabled = false; the stub errors if it is ever consulted.
        let (legacy_tx, mut rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Error,
            },
            requester.supervisor(),
            false,
        );

        let response = composite
            .ready()
            .await?
            .call(Request::TransactionsById(HashSet::from([transaction.id])))
            .await?;
        match response {
            Response::Transactions(items) => {
                assert!(matches!(items[0], InventoryResponse::Available(_)));
            }
            other => panic!("unexpected response: {other:?}"),
        }
        // The legacy peer set was never consulted in Zakura-only mode.
        assert!(rx_legacy.try_recv().is_err());

        advertiser.shutdown().await;
        requester.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn dual_stack_zakura_only_routes_chain_sync_and_mempool_requests() -> Result<(), BoxError>
    {
        let _guard = zebra_test::init();
        let block = Arc::new(Block::zcash_deserialize(
            BLOCK_TESTNET_141042_BYTES.as_slice(),
        )?);
        let transaction = UnminedTx::from(empty_v5_transaction(33));
        let (responder, mut pushed_rx) =
            normal_network_node(220, block.clone(), transaction.clone()).await?;
        let requester = ZakuraTestNode::builder(221).spawn().await?;
        requester
            .connect_native(&responder, Duration::from_secs(5))
            .await?;

        // legacy_enabled = false; the stub errors if the legacy peer set is ever
        // consulted. Chain-sync discovery and mempool data used to be routed to
        // the legacy peer set (`Passthrough`), so a node whose only peer is over
        // Zakura could never obtain tips, fetch blocks, or push transactions.
        let (legacy_tx, mut rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Error,
            },
            requester.supervisor(),
            false,
        );

        let find_blocks = composite
            .ready()
            .await?
            .call(Request::FindBlocks {
                known_blocks: vec![block_hash(1)],
                stop: Some(block.hash()),
            })
            .await?;
        match find_blocks {
            Response::BlockHashes(hashes) => assert_eq!(hashes, vec![block.hash()]),
            other => panic!("unexpected FindBlocks response: {other:?}"),
        }

        let find_headers = composite
            .ready()
            .await?
            .call(Request::FindHeaders {
                known_blocks: vec![block_hash(1)],
                stop: Some(block.hash()),
            })
            .await?;
        assert!(
            matches!(find_headers, Response::BlockHeaders(ref headers) if headers.len() == 1),
            "FindHeaders should be served over Zakura, got {find_headers:?}",
        );

        let mempool_ids = composite
            .ready()
            .await?
            .call(Request::MempoolTransactionIds)
            .await?;
        assert!(
            matches!(mempool_ids, Response::TransactionIds(ref ids) if *ids == vec![transaction.id]),
            "MempoolTransactionIds should be served over Zakura, got {mempool_ids:?}",
        );

        composite
            .ready()
            .await?
            .call(Request::PushTransaction(transaction.clone()))
            .await?;
        let pushed = pushed_rx
            .recv()
            .await
            .expect("transaction pushed over Zakura");
        assert_eq!(pushed, transaction.id);

        // None of these requests consulted the legacy peer set.
        assert!(
            rx_legacy.try_recv().is_err(),
            "Zakura-only mode must not consult the legacy peer set for serviceable requests",
        );

        responder.shutdown().await;
        requester.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn dual_stack_passthrough_request_reaches_legacy() -> Result<(), BoxError> {
        let _guard = zebra_test::init();
        let node = ZakuraTestNode::builder(208).spawn().await?;
        let (legacy_tx, mut rx_legacy) = tokio::sync::mpsc::unbounded_channel();
        let mut composite = ZakuraDualStackService::new(
            DualStackLegacyStub {
                tx: legacy_tx,
                inventory: StubInventory::Missing,
            },
            node.supervisor(),
            true,
        );

        composite.ready().await?.call(Request::Peers).await?;
        match recv_request(&mut rx_legacy).await? {
            Request::Peers => {}
            request => panic!("unexpected passthrough request: {request:?}"),
        }

        node.shutdown().await;
        Ok(())
    }
}
