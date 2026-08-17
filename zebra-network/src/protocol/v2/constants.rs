//! Constants for the version 2 Zcash P2P network protocol.

use std::time::Duration;

use zebra_chain::serialization::{MAX_HEADERS_PER_MESSAGE, MAX_PROTOCOL_MESSAGE_LEN};

use crate::protocol::external::{types::Version, MAX_INV_IN_RECEIVED_MESSAGE};

/// The ALPN protocol identifier for Mainnet connections.
pub const ALPN_MAINNET: &[u8] = b"zcash/main";

/// The ALPN protocol identifier for Testnet connections.
pub const ALPN_TESTNET: &[u8] = b"zcash/test";

/// The ALPN protocol identifier for Regtest connections.
pub const ALPN_REGTEST: &[u8] = b"zcash/regtest";

/// The maximum length of a record payload, and of any individually
/// length-prefixed element in a request or response, such as a serialized
/// block.
///
/// A length prefix exceeding this limit is a connection error of type
/// [`ErrorCode::Flood`](super::types::ErrorCode::Flood).
///
/// This is [`MAX_PROTOCOL_MESSAGE_LEN`] (2 MiB), so [`CompactSizeMessage`]
/// length prefixes enforce it automatically.
///
/// [`CompactSizeMessage`]: zebra_chain::serialization::CompactSizeMessage
pub const MAX_RECORD_PAYLOAD_LEN: usize = MAX_PROTOCOL_MESSAGE_LEN;

/// The maximum number of block locator hashes in a `get-headers` request.
pub const MAX_LOCATOR_HASHES: usize = 101;

/// The maximum number of headers in a `get-headers` response
/// (`MAX_HEADERS_RESULTS`): the same limit as legacy `headers` messages.
///
/// A response with more headers incurs a misbehavior penalty of
/// [`MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED`] points.
pub const MAX_HEADERS_RESULTS: usize = MAX_HEADERS_PER_MESSAGE;

/// The maximum number of block hashes in a `get-blocks` request.
pub const MAX_GET_BLOCKS_HASHES: usize = 128;

/// The maximum number of transaction references in a `get-tx` request.
pub const MAX_GET_TX_REFS: usize = 50_000;

/// The maximum number of address records in a `get-addr` response: the same
/// limit as legacy `addr` messages.
///
/// A response with more address records incurs a misbehavior penalty of
/// [`MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED`] points.
pub const MAX_ADDRS_IN_RESPONSE: usize = crate::constants::MAX_ADDRS_IN_MESSAGE;

/// The maximum number of transaction references in a `get-mempool` response.
///
/// The draft ZIP does not specify a limit for this response; this
/// implementation-defined bound matches the legacy `inv` message limit, and
/// bounds the memory allocated while reading untrusted response data.
//
// The legacy limit is 50,000, which fits in a `usize` on every supported
// platform.
pub const MAX_MEMPOOL_RESPONSE_REFS: usize = MAX_INV_IN_RECEIVED_MESSAGE as usize;

/// The maximum number of transaction references sent in a single
/// `get-mempool` response record.
///
/// This is an implementation-defined bound: references are 65 bytes at
/// most, so a record of this many stays under [`MAX_RECORD_PAYLOAD_LEN`],
/// and under the [`MAX_MEMPOOL_RESPONSE_REFS`] reading limit. A snapshot
/// with more references spans multiple records.
pub const MAX_MEMPOOL_RECORD_REFS: usize = 25_000;

/// The maximum number of block hashes requested by a `get-hashes` request.
pub const MAX_GET_HASHES_COUNT: usize = 50_000;

/// The maximum number of blocks requested by a `get-block-range` request.
pub const MAX_GET_BLOCK_RANGE_COUNT: usize = 65_536;

/// The maximum total serialized size of the blocks requested by a
/// `get-block-range` request, in bytes (64 MiB).
pub const MAX_GET_BLOCK_RANGE_BYTES: u64 = 67_108_864;

/// The number of concurrently pending inbound v2 handshakes above which new
/// connection attempts must validate their source address with a retry
/// token before they are accepted.
///
/// Below the threshold, connections accept in one round trip. Above it — as
/// under a handshake flood — an attempt from an unvalidated address costs
/// this node only a stateless retry packet, not a TLS handshake, and
/// spoofed-source attempts never reach the handshake at all.
pub const MAX_PENDING_INBOUND_HANDSHAKES: usize = 32;

/// The maximum number of `get-block-range` streams served concurrently to
/// one peer.
///
/// This is an implementation-defined bound: each bulk stream commits this
/// node to up to [`MAX_GET_BLOCK_RANGE_BYTES`] of block reads and transfer,
/// and a synchronizing peer spreads its work units across many peers, so
/// streams beyond the bound are refused rather than queued.
pub const MAX_CONCURRENT_BULK_STREAMS: usize = 2;

/// The maximum number of entries requested by a `get-tree-roots` request.
pub const MAX_GET_TREE_ROOTS_COUNT: usize = 4_000;

/// The maximum number of bytes requested by a `get-object` request (32 MiB).
///
/// Artifacts are expected to be divided into pieces no larger than this, so
/// that each piece is independently fetchable and verifiable.
pub const MAX_GET_OBJECT_LENGTH: u64 = 33_554_432;

/// The maximum total number of transactions (`ids_count + prefilled_count`)
/// in a compact block.
///
/// A compact block exceeding this limit is a connection error of type
/// [`ErrorCode::Flood`](super::types::ErrorCode::Flood).
pub const MAX_COMPACT_BLOCK_TX_COUNT: u64 = 65_536;

/// The maximum absolute index of a prefilled transaction in a compact block.
pub const MAX_PREFILLED_TX_INDEX: u64 = 65_535;

/// The minimum number of concurrent bidirectional streams a node should allow
/// its peer to have open.
pub const MIN_CONCURRENT_BIDI_STREAMS: u32 = 32;

/// The minimum number of concurrent unidirectional streams a node should
/// allow its peer to have open.
pub const MIN_CONCURRENT_UNI_STREAMS: u32 = 8;

/// The minimum idle timeout for connections a node wishes to retain.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The interval between transport keep-alives on connections a node wishes
/// to retain.
///
/// This is an implementation-defined value, chosen to be well under
/// [`IDLE_TIMEOUT`].
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(45);

/// The maximum time an inbound stream may make no progress before it is
/// abandoned.
///
/// A peer that opens a stream must send its stream type byte, a peer that
/// opens a request stream must send its request, and a peer that starts a
/// record must finish it, all within this time. Serving one inbound request
/// stream is bounded by it as well, because the requester gives up after
/// [`REQUEST_TIMEOUT`](crate::constants::REQUEST_TIMEOUT).
///
/// Streams are read by their own tasks, and transport keep-alives stop
/// [`IDLE_TIMEOUT`] from firing on an otherwise live connection, so without
/// this bound a stalled stream would pin its task and its buffers for the
/// life of the connection.
pub const INBOUND_STREAM_TIMEOUT: Duration =
    Duration::from_secs(2 * crate::constants::REQUEST_TIMEOUT.as_secs());

/// The number of consecutive request timeouts that disconnect a peer.
///
/// The transport provides keep-alives, so heartbeats are answered locally and
/// cannot detect a peer whose QUIC stack is alive but whose application never
/// answers a request stream. Consecutive timeouts are that peer's only
/// signature, so they disconnect it here instead: without this, it would stay
/// in the peer set and sink every request routed to it for the process
/// lifetime.
///
/// More than one timeout is required, so that a single slow response does not
/// disconnect an otherwise healthy peer.
pub const MAX_CONSECUTIVE_REQUEST_TIMEOUTS: u32 = 3;

/// The minimum peer protocol version of the version 2 protocol.
///
/// The protocol version from which the version 2 protocol is deployed has not
/// yet been assigned; it will be assigned to a network upgrade according to
/// the draft ZIP's assignment procedure. Until then, this placeholder is the
/// current network protocol version.
//
// TODO: replace with the assigned version when the draft ZIP is assigned a
//       network upgrade and protocol version.
pub const MIN_V2_PROTOCOL_VERSION: Version = crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION;

/// The misbehavior penalty for exceeding a `SHOULD`-level response size limit:
/// a `get-headers` response with more than [`MAX_HEADERS_RESULTS`] headers,
/// a non-contiguous `get-headers` response, or a `get-addr` response with
/// more than [`MAX_ADDRS_IN_RESPONSE`] address records.
pub const MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED: u32 = 20;

/// The misbehavior penalty for an announcement whose block header fails its
/// own proof of work.
///
/// The announcement is provable misbehavior on its own: the header is
/// received data, and its Equihash solution and hash are checked against
/// the header itself, independent of any chain state. The penalty reaches
/// the disconnection threshold immediately.
pub const MISBEHAVIOR_PENALTY_INVALID_POW: u32 = crate::constants::MAX_PEER_MISBEHAVIOR_SCORE;

/// The maximum number of peers this node requests high-bandwidth compact
/// block announcements from, by setting `announce = 1` in its `init` record
/// on at most this many connections at a time.
pub const MAX_HIGH_BANDWIDTH_PEERS: usize = 3;

/// The mean of the random per-connection transaction trickle delay.
///
/// Transaction announcements, and `get-mempool` subscription updates, are
/// not sent immediately: they are batched and flushed after a random
/// exponentially distributed delay, to impede network topology inference.
/// The mean matches the legacy reference behavior cited by the draft ZIP
/// (ZIP 204).
#[cfg(not(test))]
pub const TX_TRICKLE_MEAN_INTERVAL: Duration = Duration::from_secs(5);

/// A short trickle mean, so tests exercise the trickle path without slow
/// flushes.
#[cfg(test)]
pub const TX_TRICKLE_MEAN_INTERVAL: Duration = Duration::from_millis(25);

/// The `kind` byte of a header block announcement record.
pub const BLOCK_ANNOUNCEMENT_KIND_HEADER: u8 = 0x00;

/// The `kind` byte of a compact block announcement record.
pub const BLOCK_ANNOUNCEMENT_KIND_COMPACT: u8 = 0x01;

/// The `has_txs` byte of a `get-headers` entry whose block's transactions
/// are identified.
pub const HEADERS_ENTRY_HAS_TXS: u8 = 0x01;

/// The `has_txs` byte of a `get-headers` entry without transactions: the
/// responder does not hold the block, and the requester falls back to
/// `get-blocks`.
pub const HEADERS_ENTRY_NO_TXS: u8 = 0x00;

/// The record kind of the `init` record on the handshake stream.
pub const HANDSHAKE_RECORD_KIND_INIT: u8 = 0x00;

/// The sustained rate at which relayed addresses are accepted from one
/// connection, in addresses per second.
///
/// This is the token-bucket reference rate used by zcashd, cited by the
/// draft ZIP via ZIP 204: address floods beyond the burst capacity are
/// silently dropped, without a penalty.
pub const ADDR_TOKEN_RATE: f64 = 0.1;

/// The burst capacity of the relayed-address token bucket, in addresses.
///
/// The bucket starts full, so a new connection's initial gossip is
/// accepted.
pub const ADDR_TOKEN_BUCKET_CAPACITY: f64 = 1_000.0;

/// The interval between announcements of this node's own listener address
/// on each connection's address announcement stream.
///
/// The broadcast interval is implementation-defined; this matches the
/// approximate daily self-advertisement of legacy implementations.
//
// TODO: cite the ZIP 204 reference value once the companion update lands.
pub const SELF_ADDR_ANNOUNCEMENT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
