//! Shared helpers for the request and response encodings, and the error
//! type the whole version 2 wire layer reports.
//!
//! The draft's "Data Types and Encoding" rules apply to every count and
//! length here: a peer-supplied value is checked against its bound before it
//! is used, nothing is allocated in proportion to a declared count before the
//! data arrives, and non-canonical CompactSize encodings are rejected.

use std::io::{self, Write};

use thiserror::Error;

use zebra_chain::serialization::{
    CompactSize64, SerializationError, ZcashDeserialize, ZcashSerialize,
};

use super::{
    constants::MAX_RECORD_PAYLOAD_LEN,
    types::{ErrorCode, StreamType},
};

/// How a node responds to a [`WireError`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Violation {
    /// Close the connection with this error code.
    Close(ErrorCode),

    /// Discard the response and assign a misbehavior penalty; the connection
    /// stays open (draft, "`get-headers`", "`get-addr`").
    Misbehavior,
}

/// An error in a request or response encoding.
///
/// [`WireError::violation`] gives the draft's prescribed handling for each
/// variant.
#[derive(Error, Debug)]
pub enum WireError {
    /// A count in a request exceeds the bound the draft places on it, which
    /// the draft handles as `PROTOCOL_ERROR`, the default for a request that
    /// does not conform to its format (draft, "Request Streams").
    #[error("{field} {count} exceeds the maximum of {max}")]
    CountTooLarge {
        /// The count's field name.
        field: &'static str,
        /// The count the peer sent.
        count: u64,
        /// The bound.
        max: usize,
    },

    /// A count in a response exceeds a bound whose violation the draft
    /// handles as misbehavior rather than a connection error.
    #[error("{field} {count} exceeds the maximum of {max}")]
    TooManyResults {
        /// The count's field name.
        field: &'static str,
        /// The count the peer sent.
        count: u64,
        /// The bound.
        max: usize,
    },

    /// A length prefix exceeds the maximum record payload or element length,
    /// which the draft handles as `FLOOD` (draft, "Records").
    #[error("{field} {count} exceeds the maximum of {max}")]
    Flood {
        /// The count's field name.
        field: &'static str,
        /// The count the peer sent.
        count: u64,
        /// The bound.
        max: usize,
    },

    /// A `result`, type, or kind byte is not a defined value.
    #[error("unknown {what} {value:#04x}")]
    UnknownByte {
        /// Which byte it is.
        what: &'static str,
        /// The byte the peer sent.
        value: u8,
    },

    /// A `uint8` flag field was not 0 or 1.
    #[error("{field} must be 0 or 1, got {value}")]
    InvalidFlag {
        /// The field name.
        field: &'static str,
        /// The value the peer sent.
        value: u8,
    },

    /// Bytes followed a complete request, response, or element.
    #[error("{count} trailing bytes after {what}")]
    TrailingBytes {
        /// What the bytes followed.
        what: &'static str,
        /// The number of unexpected bytes.
        count: usize,
    },

    /// The stream finished in the middle of a record, request, response, or entry.
    #[error("stream finished with an incomplete {what}")]
    Truncated {
        /// What was incomplete.
        what: &'static str,
    },

    /// A record or element breaks a structural rule of its format that has no
    /// field of its own (draft, "Announcement Streams").
    #[error("nonconforming {what}: {reason}")]
    Nonconforming {
        /// What was being decoded.
        what: &'static str,
        /// Which rule it broke.
        reason: &'static str,
    },

    /// A stream type this node has no request format for (draft, "Stream Types").
    #[error("no request format for stream type {stream_type}")]
    NotARequest {
        /// The stream type.
        stream_type: StreamType,
    },

    /// A value the serialization layer rejected: truncated, non-canonical, or
    /// an invalid block or transaction.
    #[error("malformed {what}: {source}")]
    Malformed {
        /// What was being decoded.
        what: &'static str,
        /// The underlying error.
        #[source]
        source: SerializationError,
    },

    /// This node tried to send a count or length above a bound it must
    /// respect; a local bug, not peer misbehaviour.
    #[error("local {field} {count} exceeds the maximum of {max}")]
    LocalCountTooLarge {
        /// The count's field name.
        field: &'static str,
        /// The offending count.
        count: usize,
        /// The bound.
        max: usize,
    },

    /// This node tried to send a value that cannot be encoded, such as an
    /// address without the fields the record requires; a local bug, not peer
    /// misbehaviour.
    #[error("local {what} can not be encoded: {reason}")]
    LocalUnencodable {
        /// What was being encoded.
        what: &'static str,
        /// Why it can not be encoded.
        reason: &'static str,
    },

    /// The underlying stream failed or was reset while decoding, which the
    /// stream's owner interprets (not a protocol violation); or an encoder's
    /// writer failed, which the in-memory `Vec<u8>` writers every encoder
    /// uses never do.
    #[error("stream I/O error: {0}")]
    Io(#[from] io::Error),
}

impl WireError {
    /// The draft's prescribed handling of this error, or `None` for an
    /// [`Io`](Self::Io) error, which the stream's owner interprets.
    pub fn violation(&self) -> Option<Violation> {
        Some(match self {
            WireError::CountTooLarge { .. }
            | WireError::UnknownByte { .. }
            | WireError::InvalidFlag { .. }
            | WireError::TrailingBytes { .. }
            | WireError::Truncated { .. }
            | WireError::Nonconforming { .. }
            | WireError::NotARequest { .. }
            | WireError::Malformed { .. } => Violation::Close(ErrorCode::ProtocolError),
            WireError::Flood { .. } => Violation::Close(ErrorCode::Flood),
            WireError::TooManyResults { .. } => Violation::Misbehavior,
            WireError::LocalCountTooLarge { .. } | WireError::LocalUnencodable { .. } => {
                Violation::Close(ErrorCode::InternalError)
            }
            WireError::Io(_) => return None,
        })
    }

    /// The code a connection is closed with when this error ends it: the
    /// violation's code, `PROTOCOL_ERROR` for a misbehaviour (only a stream
    /// that is closing anyway reports one here), and `NO_ERROR` for a
    /// transport failure, which is not attributable to the peer.
    pub fn close_code(&self) -> ErrorCode {
        match self.violation() {
            Some(Violation::Close(code)) => code,
            Some(Violation::Misbehavior) => ErrorCode::ProtocolError,
            None => ErrorCode::NoError,
        }
    }

    /// Wraps a serialization error with the name of what was being decoded.
    pub(crate) fn malformed(what: &'static str) -> impl FnOnce(SerializationError) -> Self {
        move |source| WireError::Malformed { what, source }
    }
}

/// Reads one byte of `what` from `reader`.
pub(crate) fn read_u8(reader: &mut &[u8], what: &'static str) -> Result<u8, WireError> {
    let (&byte, rest) = reader.split_first().ok_or(WireError::Truncated { what })?;
    *reader = rest;
    Ok(byte)
}

/// Reads a 0-or-1 flag from `reader`.
pub(crate) fn read_flag(reader: &mut &[u8], field: &'static str) -> Result<bool, WireError> {
    match read_u8(reader, field)? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(WireError::InvalidFlag { field, value }),
    }
}

/// Reads a canonical CompactSize from `reader`.
///
/// Canonicity and short input are both the serialization layer's errors, and
/// both are `PROTOCOL_ERROR`.
fn read_compact_size(reader: &mut &[u8], what: &'static str) -> Result<u64, WireError> {
    CompactSize64::zcash_deserialize(reader)
        .map(u64::from)
        .map_err(WireError::malformed(what))
}

/// Reads a CompactSize count from `reader` and checks it against `max`,
/// reporting a violation with `too_large` if it is over.
///
/// The check happens before any of the counted items are read, so an
/// oversized claim costs nothing.
pub(crate) fn read_count(
    reader: &mut &[u8],
    field: &'static str,
    max: usize,
    too_large: fn(&'static str, u64, usize) -> WireError,
) -> Result<usize, WireError> {
    let count = read_compact_size(reader, field)?;
    // `max` is a small constant, so this comparison is exact on every target.
    if count > max as u64 {
        return Err(too_large(field, count, max));
    }
    // `count` is at most `max`, which is a `usize`, so the cast is lossless.
    Ok(count as usize)
}

/// Deserializes a `T` that must fill `bytes` exactly.
pub(crate) fn deserialize_exact<T: ZcashDeserialize>(
    mut bytes: &[u8],
    what: &'static str,
) -> Result<T, WireError> {
    let value = T::zcash_deserialize(&mut bytes).map_err(WireError::malformed(what))?;
    ensure_consumed(bytes, what)?;
    Ok(value)
}

/// Reports any bytes left in `cursor` as trailing data after `what`.
pub(crate) fn ensure_consumed(cursor: &[u8], what: &'static str) -> Result<(), WireError> {
    if cursor.is_empty() {
        Ok(())
    } else {
        Err(WireError::TrailingBytes {
            what,
            count: cursor.len(),
        })
    }
}

/// Writes `count` as a CompactSize after checking it against `max`.
pub(crate) fn write_count<W: Write>(
    writer: W,
    field: &'static str,
    count: usize,
    max: usize,
) -> Result<(), WireError> {
    if count > max {
        return Err(WireError::LocalCountTooLarge { field, count, max });
    }
    write_compact_size(writer, count)
}

/// Writes `value` as a CompactSize.
pub(crate) fn write_compact_size<W: Write>(writer: W, value: usize) -> Result<(), WireError> {
    // `usize` always fits in `u64` on supported targets.
    CompactSize64::from(value as u64).zcash_serialize(writer)?;
    Ok(())
}

/// Writes `value` as a CompactSize-length-prefixed element.
pub(crate) fn write_prefixed<W: Write, T: ZcashSerialize>(
    mut writer: W,
    field: &'static str,
    value: &T,
) -> Result<(), WireError> {
    let bytes = value.zcash_serialize_to_vec()?;
    if bytes.len() > MAX_RECORD_PAYLOAD_LEN {
        return Err(WireError::LocalCountTooLarge {
            field,
            count: bytes.len(),
            max: MAX_RECORD_PAYLOAD_LEN,
        });
    }
    write_compact_size(&mut writer, bytes.len())?;
    writer.write_all(&bytes)?;
    Ok(())
}
