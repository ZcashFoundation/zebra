//! Constants defined by the version 2 protocol draft.
//!
//! Each item names the draft section it comes from. The values are literal
//! transcriptions of the draft, pinned by the tests in `v2/tests/vectors.rs`,
//! so a change here must be a deliberate response to a draft change; the two
//! the draft defines by reference to the legacy protocol,
//! [`MAX_RECORD_PAYLOAD_LEN`] and [`MAX_ADDRS_PER_RESPONSE`], are aliases of
//! the legacy constants.

use std::time::Duration;

use zebra_chain::{
    parameters::{Network, NetworkKind},
    serialization::MAX_PROTOCOL_MESSAGE_LEN,
};

/// The maximum length of a record payload and of any length-prefixed
/// element: 2 MiB, the legacy maximum message size (draft, "Records"); a
/// length prefix above it is a connection error of type
/// [`FLOOD`](crate::protocol::v2::types::ErrorCode::Flood).
pub const MAX_RECORD_PAYLOAD_LEN: usize = MAX_PROTOCOL_MESSAGE_LEN;

/// The maximum number of block locator hashes in a `get-headers` request
/// (draft, "`get-headers`").
pub const MAX_LOCATOR_HASHES: usize = 101;

/// The maximum number of entries in a `get-headers` response
/// (draft, "`get-headers`").
///
/// A response with more is discarded with a misbehavior penalty rather than
/// closing the connection.
pub const MAX_HEADERS_PER_RESPONSE: usize = 160;

/// The maximum number of hashes in a `get-blocks` request
/// (draft, "`get-blocks`").
pub const MAX_BLOCKS_PER_REQUEST: usize = 128;

/// The maximum number of transaction references in a `get-tx` request
/// (draft, "`get-tx`").
pub const MAX_TXS_PER_REQUEST: usize = 50_000;

/// The maximum number of address records in a `get-addr` response: the same
/// bound as the legacy `addr` message; a response with more is misbehavior,
/// not a connection error (draft, "`get-addr`").
pub const MAX_ADDRS_PER_RESPONSE: usize = crate::constants::MAX_ADDRS_IN_MESSAGE;

/// The length of a short transaction ID, in bytes
/// (draft, "Transaction References", "Short Transaction IDs").
pub const SHORT_TX_ID_LEN: usize = 6;

/// The length of a full transaction ID, in bytes
/// (draft, "Full Transaction IDs").
pub const FULL_TX_ID_LEN: usize = 64;

/// The minimum number of concurrently open bidirectional streams a node
/// SHOULD allow its peer (draft, "Transport Requirements").
pub const MIN_BIDIRECTIONAL_STREAMS: u32 = 32;

/// The minimum number of concurrently open unidirectional streams a node
/// SHOULD allow its peer (draft, "Transport Requirements").
///
/// Enough for the three announcement stream types plus headroom for future
/// types.
pub const MIN_UNIDIRECTIONAL_STREAMS: u32 = 8;

/// The minimum connection idle timeout a node SHOULD use
/// (draft, "Connection Management").
pub const MIN_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The ALPN protocol identifier that selects `network` on the QUIC transport
/// (draft, "QUIC Transport", "Network Identification").
///
/// A node offers exactly the identifier of its own network, and refuses any
/// connection on which ALPN did not select it.
///
/// Returns `None` for a custom testnet: the draft assigns identifiers only to
/// Mainnet, the default Testnet, and Regtest, so a node on a custom testnet
/// cannot conformantly offer any identifier. This is a known gap in the
/// draft; until it is resolved, the version 2 transport is unavailable on
/// custom testnets.
pub fn alpn_protocol_id(network: &Network) -> Option<&'static [u8]> {
    match network.kind() {
        NetworkKind::Mainnet => Some(b"zcash/main"),
        NetworkKind::Testnet if network.is_default_testnet() => Some(b"zcash/test"),
        NetworkKind::Testnet => None,
        NetworkKind::Regtest => Some(b"zcash/regtest"),
    }
}
