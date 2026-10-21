//! Stream types and application error codes of the version 2 protocol.

use std::fmt;

/// The type of a stream, carried as the first byte of every stream (draft, "Stream Types").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum StreamType {
    /// Connection handshake and control; bidirectional, opened only by the
    /// connection initiator.
    Handshake,
    /// Request block headers, optionally with transaction IDs.
    GetHeaders,
    /// Request full blocks.
    GetBlocks,
    /// Request transactions.
    GetTx,
    /// Request peer addresses.
    GetAddr,
    /// Subscribe to mempool contents.
    GetMempool,
    /// Request best-chain block hashes and per-block sync-cost metadata.
    GetHashes,
    /// Stream a contiguous range of blocks, verified against an anchor hash.
    GetBlockRange,
    /// Request per-block note commitment tree roots for a height range.
    GetTreeRoots,
    /// Request a range of a content-addressed synchronization artifact.
    GetObject,
    /// Announce new blocks.
    BlockAnnouncements,
    /// Announce new transactions.
    TransactionAnnouncements,
    /// Gossip peer addresses.
    AddressAnnouncements,
}

impl StreamType {
    /// Every known stream type, in code order; [`StreamType::from_code`] searches it.
    pub const ALL: [StreamType; 13] = [
        StreamType::Handshake,
        StreamType::GetHeaders,
        StreamType::GetBlocks,
        StreamType::GetTx,
        StreamType::GetAddr,
        StreamType::GetMempool,
        StreamType::GetHashes,
        StreamType::GetBlockRange,
        StreamType::GetTreeRoots,
        StreamType::GetObject,
        StreamType::BlockAnnouncements,
        StreamType::TransactionAnnouncements,
        StreamType::AddressAnnouncements,
    ];

    /// The one-byte wire code of this stream type.
    pub const fn code(self) -> u8 {
        match self {
            StreamType::Handshake => 0x00,
            StreamType::GetHeaders => 0x01,
            StreamType::GetBlocks => 0x02,
            StreamType::GetTx => 0x03,
            StreamType::GetAddr => 0x04,
            StreamType::GetMempool => 0x05,
            StreamType::GetHashes => 0x06,
            StreamType::GetBlockRange => 0x07,
            StreamType::GetTreeRoots => 0x08,
            StreamType::GetObject => 0x09,
            StreamType::BlockAnnouncements => 0x10,
            StreamType::TransactionAnnouncements => 0x11,
            StreamType::AddressAnnouncements => 0x12,
        }
    }

    /// The stream type with wire code `code`, or `None` for one this implementation does not
    /// know, which is not a violation: the receiver refuses the stream with
    /// [`ErrorCode::UnsupportedStreamType`] and continues, so new types deploy without version
    /// gating.
    pub const fn from_code(code: u8) -> Option<Self> {
        let mut i = 0;
        while i < Self::ALL.len() {
            if Self::ALL[i].code() == code {
                return Some(Self::ALL[i]);
            }
            i += 1;
        }
        None
    }

    /// The name of this stream type.
    ///
    /// The draft assigns identifiers only to the request stream types; the
    /// handshake and announcement names are Zebra's, for logs and metrics.
    pub const fn name(self) -> &'static str {
        match self {
            StreamType::Handshake => "handshake",
            StreamType::GetHeaders => "get-headers",
            StreamType::GetBlocks => "get-blocks",
            StreamType::GetTx => "get-tx",
            StreamType::GetAddr => "get-addr",
            StreamType::GetMempool => "get-mempool",
            StreamType::GetHashes => "get-hashes",
            StreamType::GetBlockRange => "get-block-range",
            StreamType::GetTreeRoots => "get-tree-roots",
            StreamType::GetObject => "get-object",
            StreamType::BlockAnnouncements => "block-announcements",
            StreamType::TransactionAnnouncements => "transaction-announcements",
            StreamType::AddressAnnouncements => "address-announcements",
        }
    }

    /// Whether this is a request stream type: a bidirectional stream carrying
    /// exactly one request and its response (draft, "Request Streams").
    ///
    /// The handshake stream is bidirectional but is not a request stream.
    pub const fn is_request(self) -> bool {
        matches!(self.code(), 0x01..=0x0F)
    }

    /// Whether this is an announcement stream type: a long-lived
    /// unidirectional stream carrying a sequence of records
    /// (draft, "Announcement Streams").
    pub const fn is_announcement(self) -> bool {
        matches!(self.code(), 0x10..=0x1F)
    }
}

impl fmt::Display for StreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An application error code, used when closing a connection, resetting a
/// stream, or cancelling a peer's sending direction
/// (draft, "Application Error Codes").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Graceful connection close.
    NoError,
    /// The peer violated the specification.
    ProtocolError,
    /// Stream refusal: unrecognized stream type.
    UnsupportedStreamType,
    /// The peer's protocol version is below the minimum for the current
    /// network epoch.
    Obsolete,
    /// The connection is to the node itself.
    SelfConnection,
    /// The peer exceeded a size or rate limit.
    Flood,
    /// The peer's misbehavior score reached the ban threshold.
    // TODO: construct when misbehavior penalties are reported to peers.
    #[cfg_attr(not(test), allow(dead_code))]
    Misbehavior,
    /// Stop-sending/reset: the request is no longer wanted.
    Cancelled,
    /// Stream reset: the responder declines to serve this request.
    Refused,
    /// The sender encountered an internal error.
    InternalError,
}

impl ErrorCode {
    /// The wire value; transports carry it as a 62-bit integer, so the type is
    /// `u64` although every defined code fits in a byte.
    pub const fn code(self) -> u64 {
        match self {
            ErrorCode::NoError => 0x00,
            ErrorCode::ProtocolError => 0x01,
            ErrorCode::UnsupportedStreamType => 0x02,
            ErrorCode::Obsolete => 0x03,
            ErrorCode::SelfConnection => 0x04,
            ErrorCode::Flood => 0x05,
            ErrorCode::Misbehavior => 0x06,
            ErrorCode::Cancelled => 0x07,
            ErrorCode::Refused => 0x08,
            ErrorCode::InternalError => 0x09,
        }
    }

    /// The draft's constant name for this error code.
    pub const fn name(self) -> &'static str {
        match self {
            ErrorCode::NoError => "NO_ERROR",
            ErrorCode::ProtocolError => "PROTOCOL_ERROR",
            ErrorCode::UnsupportedStreamType => "UNSUPPORTED_STREAM_TYPE",
            ErrorCode::Obsolete => "OBSOLETE",
            ErrorCode::SelfConnection => "SELF_CONNECTION",
            ErrorCode::Flood => "FLOOD",
            ErrorCode::Misbehavior => "MISBEHAVIOR",
            ErrorCode::Cancelled => "CANCELLED",
            ErrorCode::Refused => "REFUSED",
            ErrorCode::InternalError => "INTERNAL_ERROR",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
