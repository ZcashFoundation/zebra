use super::*;

/// Errors that prevent the header-sync reactor from starting.
#[derive(Debug, Error)]
pub enum HeaderSyncStartError {
    /// The configured anchor is neither genesis nor a hash-matching checkpoint.
    #[error("invalid Zakura header-sync anchor at height {anchor:?}")]
    InvalidAnchor {
        /// Rejected anchor.
        anchor: (block::Height, block::Hash),
    },

    /// Only one anchor field was configured.
    #[error("Zakura header-sync anchor_height and anchor_hash must be configured together")]
    IncompleteAnchor,
}

/// Structured wire and stateless-validation errors for stream 5.
#[derive(Debug, Error)]
pub enum HeaderSyncWireError {
    /// A payload or peer-controlled count exceeded its cap.
    #[error("Zakura header-sync payload length {actual} exceeds cap {max}")]
    OversizedPayload {
        /// Actual payload length.
        actual: usize,
        /// Maximum allowed payload length.
        max: usize,
    },

    /// A decoded header count exceeded its contract.
    #[error("Zakura header-sync header count {actual} exceeds cap {max}")]
    HeaderCountLimit {
        /// Actual header count.
        actual: usize,
        /// Maximum allowed header count.
        max: usize,
    },

    /// A locally constructed `Headers` message had a different number of size hints.
    #[error("Zakura header-sync Headers body-size count {body_sizes} does not match header count {headers}")]
    BodySizeCountMismatch {
        /// Header count.
        headers: usize,
        /// Body-size hint count.
        body_sizes: usize,
    },

    /// An inbound `Headers` response did not match an in-flight request.
    #[error("unsolicited Zakura header-sync Headers response")]
    UnsolicitedHeaders,

    /// A `GetHeaders` request asked for zero headers.
    #[error("Zakura header-sync GetHeaders count must be non-zero")]
    ZeroHeaderRequestCount,

    /// A height exceeded Zebra's supported height range.
    #[error("Zakura header-sync height {0} exceeds supported range")]
    HeightOutOfRange(u32),

    /// A payload used an unknown stream-5 message discriminator.
    #[error("unknown Zakura header-sync message type {0}")]
    UnknownMessageType(u8),

    /// A frame used a message type that does not fit stream-5's u8 discriminator.
    #[error("unknown Zakura header-sync frame message type {0}")]
    UnknownFrameMessageType(u16),

    /// Frame flags are reserved in stream 5.
    #[error("unsupported Zakura header-sync frame flags {0}")]
    UnsupportedFlags(u16),

    /// Frame and payload message types disagreed.
    #[error("Zakura header-sync frame type {frame} disagrees with payload type {payload}")]
    MismatchedFrameMessageType {
        /// Outer frame message type.
        frame: u16,
        /// Inner payload message type.
        payload: u8,
    },

    /// A decoded payload had trailing bytes.
    #[error("trailing bytes in Zakura header-sync payload")]
    TrailingBytes,

    /// Adjacent headers did not hash-link.
    #[error("non-contiguous Zakura header-sync header run")]
    NonContiguousHeaders,

    /// The first header in a range did not link to its anchor.
    #[error("first Zakura header-sync range header does not link to anchor")]
    FirstHeaderDoesNotLink,

    /// Equihash solution size did not match the active network.
    #[error("Zakura header-sync Equihash solution size does not match the active network")]
    WrongEquihashSolutionSize,

    /// The compact difficulty threshold did not expand to a valid target.
    #[error("invalid Zakura header-sync compact difficulty threshold")]
    InvalidDifficultyThreshold,

    /// The header hash failed the context-free difficulty filter.
    #[error("Zakura header-sync hash {hash:?} exceeds difficulty threshold {threshold:?}")]
    DifficultyFilter {
        /// Header hash.
        hash: block::Hash,
        /// Expanded threshold from the header.
        threshold: ExpandedDifficulty,
    },

    /// A numeric conversion failed while handling bounded data.
    #[error("numeric overflow while encoding Zakura header-sync {0}")]
    NumericOverflow(&'static str),

    /// An I/O error while encoding or decoding.
    #[error("Zakura header-sync wire I/O error: {0}")]
    Io(#[from] io::Error),

    /// Zcash serialization failed.
    #[error("Zakura header-sync Zcash serialization error: {0}")]
    Serialization(#[from] SerializationError),

    /// Header time was too far in the future.
    #[error(transparent)]
    Time(#[from] BlockTimeError),

    /// Equihash verification failed.
    #[error(transparent)]
    Equihash(#[from] equihash::Error),

    /// The blocking validation task failed.
    #[error("Zakura header-sync blocking validation task failed: {0}")]
    BlockingTask(#[from] JoinError),
}
