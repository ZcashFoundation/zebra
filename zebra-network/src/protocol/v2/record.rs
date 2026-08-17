//! Record framing for the version 2 Zcash P2P network protocol.
//!
//! Each record on an announcement or handshake stream is encoded as a
//! CompactSize length prefix followed by that many bytes of payload. The same
//! length-prefixed encoding is used for individually length-prefixed elements
//! in requests and responses, such as serialized blocks.
//!
//! CompactSize encodings must be canonical: the shortest possible encoding
//! must be used for any given value, and non-canonical encodings are
//! rejected.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

use zebra_chain::{
    block,
    serialization::{CompactSize64, ZcashDeserialize, ZcashSerialize},
};

use super::{constants::MAX_RECORD_PAYLOAD_LEN, types::WireError};

/// Writes a CompactSize length prefix followed by `payload` to `writer`.
///
/// Returns an error if `payload` is longer than [`MAX_RECORD_PAYLOAD_LEN`]:
/// a conforming sender never produces such a record.
pub fn write_record<W: io::Write>(mut writer: W, payload: &[u8]) -> Result<(), WireError> {
    if payload.len() > MAX_RECORD_PAYLOAD_LEN {
        return Err(WireError::Local(format!(
            "attempted to write an over-sized record: {} bytes",
            payload.len(),
        )));
    }

    write_compact_size(&mut writer, payload.len() as u64)?;
    writer.write_all(payload)?;

    Ok(())
}

/// Writes `value` to `writer` in the canonical CompactSize encoding.
pub fn write_compact_size<W: io::Write>(mut writer: W, value: u64) -> Result<(), WireError> {
    CompactSize64::from(value).zcash_serialize(&mut writer)?;
    Ok(())
}

/// Checks a response count against a `SHOULD`-level protocol limit.
///
/// Exceeding the limit is a scored violation rather than a connection
/// error: the response is discarded, and the peer is penalized.
pub fn check_recv_limit_scored(count: u64, limit: usize, what: &str) -> Result<(), WireError> {
    if count > limit as u64 {
        return Err(WireError::Misbehavior {
            points: super::constants::MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED,
            reason: format!("{what} response with {count} entries exceeds the limit {limit}"),
        });
    }

    Ok(())
}

/// Checks an outbound collection length against a protocol limit.
///
/// Exceeding the limit is a [`WireError::Local`] error: this node built the
/// over-limit message, so the peer is never blamed for it, and the
/// connection stays open.
pub fn check_send_limit(len: usize, limit: usize, what: &str) -> Result<(), WireError> {
    if len > limit {
        return Err(WireError::Local(format!(
            "attempted to send {len} {what}; the limit is {limit}",
        )));
    }

    Ok(())
}

/// Checks an untrusted collection count from the peer against a protocol
/// limit.
///
/// Exceeding the limit is a connection error of type `PROTOCOL_ERROR`.
pub fn check_recv_limit(count: u64, limit: usize, what: &str) -> Result<(), WireError> {
    if count > limit as u64 {
        return Err(WireError::Protocol(format!(
            "{what} count {count} exceeds the limit {limit}",
        )));
    }

    Ok(())
}

/// Checks an outbound numeric request field against a protocol limit.
///
/// Exceeding the limit is a [`WireError::Local`] error: this node built the
/// over-limit request, so the peer is never blamed for it, and the
/// connection stays open.
pub fn check_send_value(value: u64, limit: u64, what: &str) -> Result<(), WireError> {
    if value > limit {
        return Err(WireError::Local(format!(
            "attempted to send a {what} of {value}; the limit is {limit}",
        )));
    }

    Ok(())
}

/// Checks an untrusted numeric field from the peer against a protocol
/// limit.
///
/// Exceeding the limit is a connection error of type `PROTOCOL_ERROR`.
pub fn check_recv_value(value: u64, limit: u64, what: &str) -> Result<(), WireError> {
    if value > limit {
        return Err(WireError::Protocol(format!(
            "{what} {value} exceeds the limit {limit}",
        )));
    }

    Ok(())
}

/// Reads a complete record from `reader`.
///
/// Returns `Ok(None)` if the stream was finished at a record boundary.
/// A stream that is finished in the middle of a record is a connection error
/// of type `PROTOCOL_ERROR`, and a length prefix exceeding
/// [`MAX_RECORD_PAYLOAD_LEN`] is a connection error of type `FLOOD`.
pub async fn read_record<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, WireError> {
    let first = match read_first_byte(reader).await? {
        Some(first) => first,
        None => return Ok(None),
    };

    Ok(Some(read_record_body(reader, first).await?))
}

/// Reads a complete record from `reader`, waiting indefinitely for the record
/// to start, but requiring the rest of it within `timeout`.
///
/// Announcement and handshake streams are long-lived and idle between
/// records, so only a record the peer has started and not finished is a
/// stall.
pub async fn read_record_timeout<R: AsyncRead + Unpin>(
    reader: &mut R,
    timeout: std::time::Duration,
) -> Result<Option<Vec<u8>>, WireError> {
    let first = match read_first_byte(reader).await? {
        Some(first) => first,
        None => return Ok(None),
    };

    let payload = tokio::time::timeout(timeout, read_record_body(reader, first))
        .await
        .map_err(|_elapsed| WireError::Timeout("a partially sent record".to_string()))??;

    Ok(Some(payload))
}

/// Reads the first byte of a record, or `Ok(None)` if the stream was finished
/// at a record boundary.
async fn read_first_byte<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<u8>, WireError> {
    let mut first = [0u8; 1];
    let n = reader.read(&mut first).await?;
    if n == 0 {
        return Ok(None);
    }

    Ok(Some(first[0]))
}

/// Reads the remainder of a record whose length prefix starts with `first`.
async fn read_record_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    first: u8,
) -> Result<Vec<u8>, WireError> {
    let len = read_compact_size_body(reader, first).await?;

    if len > MAX_RECORD_PAYLOAD_LEN as u64 {
        return Err(WireError::Flood(format!(
            "record length {len} exceeds the maximum record payload length",
        )));
    }

    read_exact_payload(reader, len as usize).await
}

/// Reads a CompactSize length prefix followed by that many bytes from
/// `reader`.
///
/// A length exceeding [`MAX_RECORD_PAYLOAD_LEN`] is a connection error of
/// type `FLOOD`; a length exceeding the tighter `max_len` is a connection
/// error of type `PROTOCOL_ERROR`.
pub async fn read_length_prefixed_bytes<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> Result<Vec<u8>, WireError> {
    let len = read_compact_size(reader).await?;

    if len > MAX_RECORD_PAYLOAD_LEN as u64 {
        return Err(WireError::Flood(format!(
            "length prefix {len} exceeds the maximum record payload length",
        )));
    }
    if len > max_len as u64 {
        return Err(WireError::Protocol(format!(
            "length prefix {len} exceeds the limit {max_len} for this element",
        )));
    }

    read_exact_payload(reader, len as usize).await
}

/// The maximum number of elements to preallocate for a collection whose
/// element count comes from an untrusted peer.
///
/// A peer can inflate an element count in a few bytes, so collections are
/// preallocated up to this many elements, and grow as their elements actually
/// arrive.
pub const MAX_PREALLOCATED_ELEMENTS: u64 = 1024;

/// Returns the capacity to preallocate for an untrusted collection of `count`
/// elements.
pub fn preallocate_len(count: u64) -> usize {
    // The result is at most `MAX_PREALLOCATED_ELEMENTS`, so it fits in a
    // `usize` on every supported platform.
    count.min(MAX_PREALLOCATED_ELEMENTS) as usize
}

/// Reads a canonical CompactSize value from `reader`.
///
/// An end of stream before the first byte is a connection error of type
/// `PROTOCOL_ERROR`. Rejects non-canonical encodings with a
/// `PROTOCOL_ERROR`.
pub async fn read_compact_size<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u64, WireError> {
    match read_first_byte(reader).await? {
        Some(first) => read_compact_size_body(reader, first).await,
        None => Err(WireError::Protocol(
            "stream finished where a CompactSize value was required".to_string(),
        )),
    }
}

/// Reads the remainder of a canonical CompactSize value whose first byte is
/// `first`.
///
/// Parsing and canonicality validation are delegated to [`CompactSize64`],
/// so the canonical-encoding rules live in one place.
async fn read_compact_size_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    first: u8,
) -> Result<u64, WireError> {
    // The number of trailing bytes indicated by each flag byte.
    let trailing: usize = match first {
        0x00..=0xFC => 0,
        0xFD => 2,
        0xFE => 4,
        0xFF => 8,
    };

    let mut buf = [0u8; 9];
    buf[0] = first;
    read_exact_or_incomplete(reader, &mut buf[1..=trailing]).await?;

    let value = CompactSize64::zcash_deserialize(&buf[..=trailing])
        .map_err(|error| WireError::Protocol(format!("invalid CompactSize encoding: {error}")))?;

    Ok(value.into())
}

/// Reads `N` bytes from `reader`.
pub async fn read_array<const N: usize, R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<[u8; N], WireError> {
    let mut bytes = [0u8; N];
    read_exact_or_incomplete(reader, &mut bytes).await?;

    Ok(bytes)
}

/// Reads a single `u8` from `reader`.
pub async fn read_u8<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u8, WireError> {
    Ok(read_array::<1, _>(reader).await?[0])
}

/// Reads exactly `buf.len()` bytes from `reader`.
///
/// An end of stream mid-element is a connection error of type
/// `PROTOCOL_ERROR`.
pub async fn read_exact_or_incomplete<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<(), WireError> {
    match reader.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Err(WireError::Protocol(
            "stream finished in the middle of a record or element".to_string(),
        )),
        Err(err) => Err(err.into()),
    }
}

/// Checks that the stream is finished: any further data is a connection error
/// of type `PROTOCOL_ERROR`.
pub async fn expect_end_of_stream<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(), WireError> {
    let mut buf = [0u8; 1];
    let n = reader.read(&mut buf).await?;
    if n != 0 {
        return Err(WireError::Protocol(
            "unexpected data after a complete request or response".to_string(),
        ));
    }
    Ok(())
}

/// Reads a little-endian `u64` from `reader`.
pub async fn read_exact_u64_le<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u64, WireError> {
    Ok(u64::from_le_bytes(read_array(reader).await?))
}

/// Reads a little-endian `u32` from `reader`.
pub async fn read_exact_u32_le<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u32, WireError> {
    Ok(u32::from_le_bytes(read_array(reader).await?))
}

/// Reads a 32-byte block hash from `reader`.
pub async fn read_block_hash<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<block::Hash, WireError> {
    Ok(block::Hash(read_array(reader).await?))
}

async fn read_exact_payload<R: AsyncRead + Unpin>(
    reader: &mut R,
    len: usize,
) -> Result<Vec<u8>, WireError> {
    // `len` is already bounded by `MAX_RECORD_PAYLOAD_LEN` at every call
    // site, so this allocation is bounded.
    let mut payload = vec![0u8; len];
    read_exact_or_incomplete(reader, &mut payload).await?;
    Ok(payload)
}
