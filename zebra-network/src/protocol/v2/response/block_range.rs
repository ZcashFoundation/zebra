//! The `get-block-range` response (draft, "`get-block-range`"): a `result`
//! byte, then, if found, CompactSize-length-prefixed serialized blocks in
//! descending height order until the responder finishes the stream.
//!
//! This module does framing and the count/byte bounds only. Hash chaining,
//! header parsing and the consensus size limit are the client stream's
//! checks (`sync_peer::block_range`).

use std::io::Write;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::protocol::v2::{
    constants::MAX_RECORD_PAYLOAD_LEN,
    request::GetBlockRange,
    wire::{write_compact_size, WireError},
};

use super::{
    ensure_finished, read_byte_or_fin, read_compact_size_from, RESULT_FOUND, RESULT_NOT_FOUND,
};

/// The start of a `get-block-range` response.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlockRangeAnswer {
    /// `result = 0x00`: blocks follow.
    Found,

    /// `result = 0x02`: the responder does not have `final_hash`; nothing
    /// follows, and anything that does is trailing data.
    NotFound,
}

/// Writes the `result` byte for `answer`.
pub fn write_block_range_answer<W: Write>(
    mut writer: W,
    answer: BlockRangeAnswer,
) -> Result<(), WireError> {
    let result = match answer {
        BlockRangeAnswer::Found => RESULT_FOUND,
        BlockRangeAnswer::NotFound => RESULT_NOT_FOUND,
    };
    writer.write_all(&[result])?;
    Ok(())
}

/// Writes one element: `CompactSize len ‖ block`, where `block` is the
/// block's complete serialization.
///
/// # Errors
///
/// [`WireError::LocalUnencodable`] if `block` is longer than
/// [`MAX_RECORD_PAYLOAD_LEN`].
pub fn write_block_range_element<W: Write>(mut writer: W, block: &[u8]) -> Result<(), WireError> {
    if block.len() > MAX_RECORD_PAYLOAD_LEN {
        return Err(WireError::LocalUnencodable {
            what: "get-block-range element",
            reason: "the block is longer than the maximum element length",
        });
    }
    write_compact_size(&mut writer, block.len())?;
    writer.write_all(block)?;
    Ok(())
}

/// Reads the `result` byte. An empty stream is [`WireError::Truncated`], an
/// unknown byte [`WireError::UnknownByte`], and a byte after `NotFound`
/// [`WireError::TrailingBytes`] (all `PROTOCOL_ERROR`).
pub async fn read_block_range_answer<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<BlockRangeAnswer, WireError> {
    match read_byte_or_fin(reader).await? {
        None => Err(WireError::Truncated {
            what: "get-block-range response",
        }),
        Some(RESULT_FOUND) => Ok(BlockRangeAnswer::Found),
        Some(RESULT_NOT_FOUND) => {
            ensure_finished(reader, "get-block-range not-found response").await?;
            Ok(BlockRangeAnswer::NotFound)
        }
        Some(value) => Err(WireError::UnknownByte {
            what: "result code",
            value,
        }),
    }
}

/// The requester's running bounds for one response (draft: "both bounds are
/// exact"; spec issue S2: the byte bound is `max(max_bytes, first block)`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockRangeBounds {
    /// The request's `count`.
    pub count: u32,
    /// The request's `max_bytes`.
    pub max_bytes: u32,
    /// Blocks delivered so far.
    pub delivered: u32,
    /// Serialized bytes delivered so far, the first block included.
    pub delivered_bytes: u64,
}

impl BlockRangeBounds {
    /// The bounds for a response to `request`, with nothing delivered.
    pub fn new(request: &GetBlockRange) -> Self {
        BlockRangeBounds {
            count: request.count,
            max_bytes: request.max_bytes,
            delivered: 0,
            delivered_bytes: 0,
        }
    }

    /// Checks that one more block of `length` bytes is within the bounds.
    fn check_next(&self, length: u64) -> Result<(), WireError> {
        if self.delivered >= self.count {
            return Err(WireError::Flood {
                field: "get-block-range count",
                count: u64::from(self.delivered) + 1,
                // `u32` fits in a `usize` on every supported target, so the cast is lossless.
                max: self.count as usize,
            });
        }

        // The first block is delivered regardless of `max_bytes`, so a maximum-size block is
        // always retrievable (draft, "`get-block-range`").
        let total = self.delivered_bytes.saturating_add(length);
        if self.delivered > 0 && total > u64::from(self.max_bytes) {
            return Err(WireError::Flood {
                field: "get-block-range max_bytes",
                count: total,
                // `u32` fits in a `usize` on every supported target, so the cast is lossless.
                max: self.max_bytes as usize,
            });
        }

        Ok(())
    }

    /// Records the delivery of one block of `length` bytes.
    fn record(&mut self, length: u64) {
        // `check_next` passed, so `delivered < count <= u32::MAX`.
        self.delivered += 1;
        self.delivered_bytes = self.delivered_bytes.saturating_add(length);
    }
}

/// Reads the next element after a `Found` answer, or `None` when the
/// responder finished the stream.
///
/// # Errors
///
/// - a length prefix above `MAX_RECORD_PAYLOAD_LEN`: [`WireError::Flood`],
///   from the prefix alone;
/// - a block past `count`, or any block after the first that brings
///   `delivered_bytes` above `max_bytes`: [`WireError::Flood`], detected from
///   the prefix, before the block is read;
/// - a FIN inside an element: [`WireError::Truncated`].
///
/// On success `bounds` is updated. The returned bytes are exactly one
/// element's payload, allocated as they arrive.
pub async fn read_block_range_element<R: AsyncRead + Unpin>(
    reader: &mut R,
    bounds: &mut BlockRangeBounds,
) -> Result<Option<Bytes>, WireError> {
    const WHAT: &str = "get-block-range element";

    // A FIN between elements is the responder finishing the stream, which it may do at any
    // element boundary.
    let Some(first) = read_byte_or_fin(reader).await? else {
        return Ok(None);
    };

    // The rest of the prefix is read with the byte already taken in front of it, so the
    // serialization layer still checks canonicity.
    let length = read_compact_size_from(&mut (&[first][..]).chain(&mut *reader), WHAT).await?;

    // The limit fits in a `u64`, so the comparison is exact.
    if length > MAX_RECORD_PAYLOAD_LEN as u64 {
        return Err(WireError::Flood {
            field: "get-block-range element length",
            count: length,
            max: MAX_RECORD_PAYLOAD_LEN,
        });
    }
    bounds.check_next(length)?;

    // `read_to_end` lets the `Vec` grow as data arrives, never from the claimed length, so a
    // false prefix allocates nothing up front.
    let mut block = Vec::new();
    (&mut *reader).take(length).read_to_end(&mut block).await?;
    // `length` is at most `MAX_RECORD_PAYLOAD_LEN`, which is a `usize`, so the cast is lossless.
    if block.len() != length as usize {
        return Err(WireError::Truncated { what: WHAT });
    }

    bounds.record(length);
    Ok(Some(block.into()))
}
