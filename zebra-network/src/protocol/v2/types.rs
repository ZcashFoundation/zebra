//! Core types for the version 2 Zcash P2P network protocol:
//! stream types, application error codes, and wire errors.

use thiserror::Error;

use zebra_chain::serialization::SerializationError;

/// The type of a stream, identified by the first byte of the stream.
///
/// Stream type `0x00` and the ranges `0x01`–`0x0F` and `0x10`–`0x1F` are
/// reserved for the handshake, request stream types, and announcement stream
/// types respectively.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum StreamType {
    /// Connection handshake and control (bidirectional, initiator only).
    Handshake = 0x00,

    /// Request block headers (bidirectional).
    GetHeaders = 0x01,

    /// Request blocks, full or compact (bidirectional).
    GetBlocks = 0x02,

    /// Request transactions (bidirectional).
    GetTx = 0x03,

    /// Request peer addresses (bidirectional).
    GetAddr = 0x04,

    /// Subscribe to mempool contents (bidirectional).
    GetMempool = 0x05,

    /// Request best-chain block hashes and sync-cost metadata at a height
    /// stride (bidirectional).
    GetHashes = 0x06,

    /// Stream a contiguous range of blocks, verified against an anchor hash
    /// (bidirectional).
    GetBlockRange = 0x07,

    /// Request per-block note commitment tree roots for a height range
    /// (bidirectional).
    GetTreeRoots = 0x08,

    /// Request a range of a content-addressed synchronization artifact
    /// (bidirectional).
    GetObject = 0x09,

    /// Announce new blocks (unidirectional).
    BlockAnnouncements = 0x10,

    /// Announce new transactions (unidirectional).
    TransactionAnnouncements = 0x11,

    /// Gossip peer addresses (unidirectional).
    AddressAnnouncements = 0x12,
}

impl StreamType {
    /// Returns the stream type for `byte`, or `None` if the byte is not a
    /// recognized stream type.
    ///
    /// A stream with an unrecognized type byte must be refused with
    /// [`ErrorCode::UnsupportedStreamType`], without treating it as a
    /// connection error or assigning a misbehavior penalty.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(StreamType::Handshake),
            0x01 => Some(StreamType::GetHeaders),
            0x02 => Some(StreamType::GetBlocks),
            0x03 => Some(StreamType::GetTx),
            0x04 => Some(StreamType::GetAddr),
            0x05 => Some(StreamType::GetMempool),
            0x06 => Some(StreamType::GetHashes),
            0x07 => Some(StreamType::GetBlockRange),
            0x08 => Some(StreamType::GetTreeRoots),
            0x09 => Some(StreamType::GetObject),
            0x10 => Some(StreamType::BlockAnnouncements),
            0x11 => Some(StreamType::TransactionAnnouncements),
            0x12 => Some(StreamType::AddressAnnouncements),
            _ => None,
        }
    }

    /// Returns the wire byte for this stream type.
    pub fn byte(self) -> u8 {
        self as u8
    }

    /// Returns true if this is a request stream type.
    pub fn is_request(self) -> bool {
        matches!(
            self,
            StreamType::GetHeaders
                | StreamType::GetBlocks
                | StreamType::GetTx
                | StreamType::GetAddr
                | StreamType::GetMempool
                | StreamType::GetHashes
                | StreamType::GetBlockRange
                | StreamType::GetTreeRoots
                | StreamType::GetObject
        )
    }

    /// Returns true if this is an announcement stream type.
    pub fn is_announcement(self) -> bool {
        matches!(
            self,
            StreamType::BlockAnnouncements
                | StreamType::TransactionAnnouncements
                | StreamType::AddressAnnouncements
        )
    }
}

/// The SHA-256 content hash identifying a synchronization artifact in
/// `get-object` requests.
///
/// Artifacts are immutable, content-addressed objects (for example,
/// known-hash chunk files and state snapshot pieces); the protocol does not
/// interpret their contents.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct ObjectHash(pub [u8; 32]);

/// Application error codes, used when closing a connection, resetting a
/// stream, or cancelling a peer's sending direction.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u64)]
pub enum ErrorCode {
    /// Graceful connection close.
    NoError = 0x00,

    /// The peer violated the protocol specification.
    ProtocolError = 0x01,

    /// Stream refusal: unrecognized stream type.
    UnsupportedStreamType = 0x02,

    /// The peer's protocol version is below the minimum for the current
    /// network epoch.
    Obsolete = 0x03,

    /// The connection is to the node itself.
    SelfConnection = 0x04,

    /// The peer exceeded a size or rate limit.
    Flood = 0x05,

    /// The peer's misbehavior score reached the ban threshold.
    Misbehavior = 0x06,

    /// Stop-sending/reset: the request is no longer wanted.
    Cancelled = 0x07,

    /// Stream reset: the responder declines to serve this request.
    Refused = 0x08,

    /// The sender encountered an internal error.
    InternalError = 0x09,
}

impl ErrorCode {
    /// Returns the error code for a wire code.
    ///
    /// An unrecognized code is treated as [`ErrorCode::InternalError`],
    /// as required by the specification.
    pub fn from_wire(code: u64) -> Self {
        match code {
            0x00 => ErrorCode::NoError,
            0x01 => ErrorCode::ProtocolError,
            0x02 => ErrorCode::UnsupportedStreamType,
            0x03 => ErrorCode::Obsolete,
            0x04 => ErrorCode::SelfConnection,
            0x05 => ErrorCode::Flood,
            0x06 => ErrorCode::Misbehavior,
            0x07 => ErrorCode::Cancelled,
            0x08 => ErrorCode::Refused,
            0x09 => ErrorCode::InternalError,
            _ => ErrorCode::InternalError,
        }
    }

    /// Returns the wire code for this error code.
    pub fn wire_code(self) -> u64 {
        self as u64
    }
}

/// An error reading or validating version 2 protocol data.
///
/// Each variant maps to the action the node must take:
/// connection errors carry the [`ErrorCode`] to close the connection with,
/// [`WireError::Misbehavior`] assigns a score without disconnecting, and
/// [`WireError::Local`] is this node's own fault, so it never closes the
/// connection.
#[derive(Error, Debug)]
pub enum WireError {
    /// The peer violated the specification: a connection error of type
    /// [`ErrorCode::ProtocolError`].
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The peer exceeded a size limit: a connection error of type
    /// [`ErrorCode::Flood`].
    #[error("flood: {0}")]
    Flood(String),

    /// The peer violated a limit that incurs a misbehavior score instead of
    /// immediate disconnection.
    #[error("misbehavior ({points} points): {reason}")]
    Misbehavior {
        /// The misbehavior score to assign.
        points: u32,
        /// The reason for the penalty.
        reason: String,
    },

    /// The peer stopped making progress part-way through a stream or record:
    /// a connection error of type [`ErrorCode::ProtocolError`].
    #[error("timed out waiting for the peer to finish {0}")]
    Timeout(String),

    /// This node could not encode a valid request or response.
    ///
    /// The remote peer is not at fault: the connection is left open, and only
    /// the request or the stream fails.
    #[error("local error: {0}")]
    Local(String),

    /// An inner Zcash structure failed to parse: a connection error of type
    /// [`ErrorCode::ProtocolError`].
    #[error("parse error: {0}")]
    Serialization(#[from] SerializationError),

    /// An I/O error reading from or writing to the transport.
    ///
    /// An unexpected end of stream is a connection error of type
    /// [`ErrorCode::ProtocolError`]; other I/O errors are local transport
    /// failures.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl WireError {
    /// Returns the application error code a node closing the connection due
    /// to this error should use, or `None` if this error does not close the
    /// connection.
    pub fn connection_error_code(&self) -> Option<ErrorCode> {
        match self {
            WireError::Protocol(_) | WireError::Timeout(_) | WireError::Serialization(_) => {
                Some(ErrorCode::ProtocolError)
            }
            WireError::Flood(_) => Some(ErrorCode::Flood),
            WireError::Misbehavior { .. } | WireError::Local(_) => None,
            WireError::Io(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                Some(ErrorCode::ProtocolError)
            }
            WireError::Io(_) => Some(ErrorCode::InternalError),
        }
    }
}
