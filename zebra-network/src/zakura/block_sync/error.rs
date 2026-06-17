use super::*;

/// Structured wire and stateless-validation errors for stream 6.
#[derive(Debug, Error)]
pub enum BlockSyncWireError {
    /// A payload or peer-controlled count exceeded its cap.
    #[error("Zakura block-sync payload length {actual} exceeds cap {max}")]
    OversizedPayload {
        /// Actual payload length.
        actual: usize,
        /// Maximum allowed payload length.
        max: usize,
    },

    /// A decoded request or response block count exceeded its contract.
    #[error("Zakura block-sync block count {actual} exceeds cap {max}")]
    BlockCountLimit {
        /// Actual count.
        actual: u32,
        /// Maximum allowed count.
        max: u32,
    },

    /// A `GetBlocks`, `BlocksDone`, or `RangeUnavailable` count was zero.
    #[error("Zakura block-sync count must be non-zero")]
    ZeroBlockCount,

    /// A decoded block body exceeded the consensus block-size limit.
    #[error("Zakura block-sync block length {actual} exceeds consensus cap {max}")]
    OversizedBlock {
        /// Actual serialized block length.
        actual: usize,
        /// Maximum allowed serialized block length.
        max: usize,
    },

    /// A height exceeded Zebra's supported height range.
    #[error("Zakura block-sync height {0} exceeds supported range")]
    HeightOutOfRange(u32),

    /// A payload used an unknown stream-6 message discriminator.
    #[error("unknown Zakura block-sync message type {0}")]
    UnknownMessageType(u8),

    /// A frame used a message type that does not fit stream-6's u8 discriminator.
    #[error("unknown Zakura block-sync frame message type {0}")]
    UnknownFrameMessageType(u16),

    /// Frame flags are reserved in stream 6.
    #[error("unsupported Zakura block-sync frame flags {0}")]
    UnsupportedFlags(u16),

    /// Frame and payload message types disagreed.
    #[error("Zakura block-sync frame type {frame} disagrees with payload type {payload}")]
    MismatchedFrameMessageType {
        /// Outer frame message type.
        frame: u16,
        /// Inner payload message type.
        payload: u8,
    },

    /// A decoded payload had trailing bytes.
    #[error("trailing bytes in Zakura block-sync payload")]
    TrailingBytes,

    /// A numeric conversion failed while handling bounded data.
    #[error("numeric overflow while encoding Zakura block-sync {0}")]
    NumericOverflow(&'static str),

    /// An I/O error while encoding or decoding.
    #[error("Zakura block-sync wire I/O error: {0}")]
    Io(#[from] io::Error),

    /// Zcash serialization failed.
    #[error("Zakura block-sync Zcash serialization error: {0}")]
    Serialization(#[from] SerializationError),
}
