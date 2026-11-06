//! Responses on the request stream types `get-headers`, `get-blocks`,
//! `get-tx`, `get-addr`, and `get-object` (draft, "Request Stream Types").
//!
//! Block and transaction responses are open sequences of entries the
//! responder may cut short after any complete entry, and header responses are
//! a counted sequence; those three are read entry by entry from the response
//! stream, so each entry is parsed once its bytes have arrived and nothing is
//! allocated from a peer's length prefix. Address responses are small and
//! bounded, and are decoded from the complete body. Object responses are a
//! short header and then raw data until the stream finishes, read as it
//! arrives up to the request's bound.

use std::{io, io::Write, sync::Arc};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt};

use zebra_chain::{
    block,
    serialization::{CompactSize64, ZcashDeserialize, ZcashSerialize},
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{canonical_peer_addr, AddrV2},
};

use super::{
    constants::{MAX_ADDRS_PER_RESPONSE, MAX_HEADERS_PER_RESPONSE, MAX_RECORD_PAYLOAD_LEN},
    wire::{
        deserialize_exact, ensure_consumed, read_count, write_count, write_prefixed, WireError,
    },
};

pub mod block_range;
pub mod hashes;

/// The `result` byte that introduces a found object.
pub const RESULT_FOUND: u8 = 0x00;

/// The `result` byte for an object the responder does not have.
pub const RESULT_NOT_FOUND: u8 = 0x02;

/// How many bytes of a `get-object` answer's data are read from the stream
/// at a time.
const OBJECT_DATA_CHUNK_LEN: usize = 64 * 1024;

/// The start of a `get-object` response (draft, "`get-object`"): whether the
/// responder has the object, and if so its total size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectAnswer {
    /// The responder has the object: `size` is its total length in bytes, as
    /// the peer claims it, and the requested bytes follow until FIN.
    Found {
        /// The object's total size, unauthenticated until the whole object
        /// has been hashed.
        size: u64,
    },

    /// The responder does not have the object; nothing follows.
    NotFound,
}

/// Encodes the start of a `get-object` response: the `result` byte, then the
/// object's size as a CompactSize if it was found. The object bytes are
/// written after it.
pub fn encode_object_header(answer: ObjectAnswer) -> Result<Vec<u8>, WireError> {
    let mut header = Vec::new();
    match answer {
        ObjectAnswer::Found { size } => {
            header.push(RESULT_FOUND);
            CompactSize64::from(size).zcash_serialize(&mut header)?;
        }
        ObjectAnswer::NotFound => header.push(RESULT_NOT_FOUND),
    }
    Ok(header)
}

/// Reads the start of a `get-object` response (draft, "`get-object`"): the
/// `result` byte, and the object's size if the responder has it.
///
/// A response that ends before its `result` byte or inside `size`, an
/// unknown `result`, and a non-canonical `size` are `PROTOCOL_ERROR`s. A
/// not-found answer is complete in its `result` byte, so this reads on to the
/// end of the stream, and anything after it is trailing data. A found
/// answer's data follows; [`read_object_data`] reads it.
pub async fn read_object_header<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<ObjectAnswer, WireError> {
    let mut result = [0];
    read_exact_or_truncated(reader, &mut result, "get-object response").await?;
    match result[0] {
        RESULT_FOUND => {
            let size = read_compact_size_from(reader, "get-object size").await?;
            Ok(ObjectAnswer::Found { size })
        }
        RESULT_NOT_FOUND => {
            ensure_finished(reader, "get-object not-found answer").await?;
            Ok(ObjectAnswer::NotFound)
        }
        value => Err(WireError::UnknownByte {
            what: "result code",
            value,
        }),
    }
}

/// Reads a found `get-object` answer's data until the responder finishes the
/// stream, appending it to `data`.
///
/// `limit` is the most data the answer may carry: the request's length, and
/// no further than the end of the object as the answer's own `size` gives it
/// (see [`GetObject::data_limit`](super::request::GetObject::data_limit)).
/// A byte beyond it is a connection error of type `FLOOD` (draft,
/// "`get-object`"), detected in the read that delivers it, before the read's
/// bytes are appended, so this never appends more than `limit` bytes. `data`
/// grows as bytes arrive, never in advance from the peer's `size`.
///
/// If the read fails, or the future is dropped, `data` holds the bytes read
/// so far: a prefix of the answer's data.
pub async fn read_object_data<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: u32,
    data: &mut Vec<u8>,
) -> Result<(), WireError> {
    // A `u32` fits in a `usize` on every supported target.
    let limit = limit as usize;
    let start = data.len();
    // Room for one byte past the limit, so a limit of zero still sees a flood.
    let mut chunk = vec![0; OBJECT_DATA_CHUNK_LEN.min(limit.saturating_add(1))];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        // `data` only grows here, from `start`.
        let received = (data.len() - start).saturating_add(read);
        if received > limit {
            return Err(WireError::Flood {
                field: "get-object data",
                // `usize` always fits in `u64` on supported targets.
                count: received as u64,
                max: limit,
            });
        }
        data.extend_from_slice(&chunk[..read]);
    }
}

/// One entry of a `get-blocks` or `get-tx` response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectResult<T> {
    /// The responder had the object.
    Found(T),

    /// The responder did not have the object.
    NotFound,
}

impl<T: ZcashSerialize> ObjectResult<T> {
    /// Writes the entry: the `result` byte, then the length-prefixed object
    /// if found.
    pub fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            ObjectResult::Found(object) => {
                writer.write_all(&[RESULT_FOUND])?;
                write_prefixed(writer, "object", object)?;
            }
            ObjectResult::NotFound => writer.write_all(&[RESULT_NOT_FOUND])?,
        }
        Ok(())
    }
}

/// Reads one byte, or `None` if the stream finished cleanly instead.
async fn read_byte_or_fin<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<u8>, WireError> {
    let mut byte = [0];
    match reader.read(&mut byte).await? {
        0 => Ok(None),
        _ => Ok(Some(byte[0])),
    }
}

/// Fills `buf` from `reader`; a FIN before it is full is a truncated `what`.
async fn read_exact_or_truncated<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), WireError> {
    reader.read_exact(buf).await.map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            WireError::Truncated { what }
        } else {
            WireError::Io(error)
        }
    })?;
    Ok(())
}

/// Reads a canonical CompactSize from `reader`: the first byte, then the
/// 0, 2, 4, or 8 bytes it announces, decoded by the serialization layer so
/// canonicity is still its check.
async fn read_compact_size_from<R: AsyncRead + Unpin>(
    reader: &mut R,
    what: &'static str,
) -> Result<u64, WireError> {
    let mut bytes = [0; 9];
    read_exact_or_truncated(reader, &mut bytes[..1], what).await?;
    let encoded_len = match bytes[0] {
        0x00..=0xFC => 1,
        0xFD => 3,
        0xFE => 5,
        0xFF => 9,
    };
    read_exact_or_truncated(reader, &mut bytes[1..encoded_len], what).await?;
    CompactSize64::zcash_deserialize(&bytes[..encoded_len])
        .map(u64::from)
        .map_err(WireError::malformed(what))
}

/// Reads a CompactSize-length-prefixed element from `reader`.
///
/// A length above the maximum element length is a `FLOOD` error, detected
/// from the prefix alone, before any byte of the element is read.
async fn read_prefixed_from<R: AsyncRead + Unpin>(
    reader: &mut R,
    field: &'static str,
    what: &'static str,
) -> Result<Vec<u8>, WireError> {
    let length = read_compact_size_from(reader, what).await?;
    // The limit fits in a `u64`, so the comparison is exact.
    if length > MAX_RECORD_PAYLOAD_LEN as u64 {
        return Err(WireError::Flood {
            field,
            count: length,
            max: MAX_RECORD_PAYLOAD_LEN,
        });
    }

    // `read_to_end` reserves 32 bytes once and then lets the `Vec` grow by doubling as data
    // arrives, never from the claimed length, so a false prefix allocates nothing up front; the
    // peer pays for every byte it makes this node hold.
    let mut bytes = Vec::new();
    (&mut *reader).take(length).read_to_end(&mut bytes).await?;
    // `length` is at most `MAX_RECORD_PAYLOAD_LEN`, which is a `usize`, so the cast is lossless.
    if bytes.len() != length as usize {
        return Err(WireError::Truncated { what });
    }
    Ok(bytes)
}

/// Checks that `reader` finishes here: at least one more byte is trailing
/// data after `what`. The count is not read further.
async fn ensure_finished<R: AsyncRead + Unpin>(
    reader: &mut R,
    what: &'static str,
) -> Result<(), WireError> {
    match read_byte_or_fin(reader).await? {
        None => Ok(()),
        Some(_) => Err(WireError::TrailingBytes { what, count: 1 }),
    }
}

/// Reads a `get-blocks` or `get-tx` response to a request for `expected`
/// objects (draft, "`get-blocks`", "`get-tx`").
///
/// The responder answers in order and may finish after any complete entry,
/// but an entry past the request's count is trailing data. Whether each
/// object is the one requested is the caller's check.
pub async fn read_objects<R: AsyncRead + Unpin, T: ZcashDeserialize>(
    reader: &mut R,
    expected: usize,
) -> Result<Vec<ObjectResult<Arc<T>>>, WireError> {
    let mut entries = Vec::new();
    while entries.len() < expected {
        let Some(result) = read_byte_or_fin(reader).await? else {
            return Ok(entries);
        };
        entries.push(match result {
            RESULT_FOUND => {
                let bytes = read_prefixed_from(reader, "object", "response entry").await?;
                ObjectResult::Found(deserialize_exact::<Arc<T>>(&bytes, "object")?)
            }
            RESULT_NOT_FOUND => ObjectResult::NotFound,
            value => {
                return Err(WireError::UnknownByte {
                    what: "result code",
                    value,
                })
            }
        });
    }
    ensure_finished(reader, "response").await?;
    Ok(entries)
}

/// Writes a complete `get-headers` response: the count, then the entries.
pub fn write_headers_response<W: Write>(
    mut writer: W,
    headers: &[Arc<block::Header>],
    tx_ids: bool,
) -> Result<(), WireError> {
    write_count(
        &mut writer,
        "count",
        headers.len(),
        MAX_HEADERS_PER_RESPONSE,
    )?;
    for header in headers {
        write_prefixed(&mut writer, "header", header)?;
        if tx_ids {
            // Transaction IDs are never supplied, so every entry has has_txs = 0 when the request
            // asked for them (draft, "get-headers": the responder MAY).
            writer.write_all(&[0])?;
        }
    }
    Ok(())
}

/// Reads a `get-headers` response (draft, "`get-headers`").
///
/// A count above [`MAX_HEADERS_PER_RESPONSE`] is misbehavior from the count
/// alone, before any entry is read. Only the `tx_ids = 0` layout is read:
/// this node never asks for transaction IDs. Contiguity is the caller's check.
pub async fn read_headers<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<Arc<block::Header>>, WireError> {
    let count = read_compact_size_from(reader, "headers response count").await?;
    // The limit fits in a `u64`, so the comparison is exact.
    if count > MAX_HEADERS_PER_RESPONSE as u64 {
        return Err(WireError::TooManyResults {
            field: "count",
            count,
            max: MAX_HEADERS_PER_RESPONSE,
        });
    }

    let mut headers = Vec::new();
    for _ in 0..count {
        let bytes = read_prefixed_from(reader, "header", "header entry").await?;
        headers.push(deserialize_exact::<Arc<block::Header>>(&bytes, "header")?);
    }
    ensure_finished(reader, "headers response").await?;
    Ok(headers)
}

/// Encodes a complete `get-addr` response from sanitized addresses
/// (draft, "`get-addr`").
///
/// Every address must have its services and last-seen time, as
/// [`MetaAddr::sanitize`] guarantees; one without is a local error.
pub fn encode_addr_response(addrs: &[MetaAddr]) -> Result<Bytes, WireError> {
    let mut body = Vec::new();
    write_count(&mut body, "count", addrs.len(), MAX_ADDRS_PER_RESPONSE)?;
    for addr in addrs {
        let unencodable = |reason| WireError::LocalUnencodable {
            what: "address record",
            reason,
        };
        let untrusted_services = addr
            .services()
            .ok_or(unencodable("address has no services; sanitize it first"))?;
        let untrusted_last_seen = addr.last_seen().ok_or(unencodable(
            "address has no last seen time; sanitize it first",
        ))?;
        let record = AddrV2::IpAddr {
            untrusted_last_seen,
            untrusted_services,
            addr: canonical_peer_addr(addr.addr()),
        };
        record.zcash_serialize(&mut body)?;
    }
    Ok(body.into())
}

/// Decodes a complete `get-addr` response body into gossiped addresses.
///
/// Records with a network ID Zebra does not support are skipped, as the
/// legacy `addrv2` reader does. A count above [`MAX_ADDRS_PER_RESPONSE`] is
/// reported as misbehavior from the count alone.
pub fn decode_addr_response(body: &[u8]) -> Result<Vec<MetaAddr>, WireError> {
    let mut reader = body;

    let count = read_count(
        &mut reader,
        "count",
        MAX_ADDRS_PER_RESPONSE,
        |field, count, max| WireError::TooManyResults { field, count, max },
    )?;

    let mut addrs = Vec::new();
    for _ in 0..count {
        let record = AddrV2::zcash_deserialize(&mut reader)
            .map_err(WireError::malformed("address record"))?;
        if let Ok(addr) = MetaAddr::try_from(record) {
            addrs.push(addr);
        }
    }

    ensure_consumed(reader, "addr response")?;
    Ok(addrs)
}
