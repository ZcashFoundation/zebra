//! Record framing for the version 2 protocol (draft, "Records").
//!
//! Records are the unit of data on the handshake stream, on announcement
//! streams, and in the open-ended `get-mempool` response. Each record is a
//! 4-byte little-endian `uint32` payload length followed by that many
//! payload bytes. Framing says nothing about the payload: the stream type
//! decides what a payload means, and the stream's owner decodes it after
//! framing hands over the complete record.
//!
//! [`RecordCodec`] is a [`tokio_util::codec`] decoder; the stream's owner
//! reads or writes the 1-byte stream type before framing.
//! `LengthDelimitedCodec` is not used: it reserves the claimed length as soon
//! as the prefix arrives and cannot tell `FLOOD` from `PROTOCOL_ERROR`.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::Decoder;

use super::{constants::MAX_RECORD_PAYLOAD_LEN, wire::WireError};

/// A 4-byte little-endian `uint32` payload length prefix (draft, "Records").
const RECORD_LENGTH_PREFIX_LEN: usize = 4;

/// What a decoder does with a record that is longer than its stream's records
/// can be, but within [`MAX_RECORD_PAYLOAD_LEN`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Oversized {
    /// The record does not conform to its stream type's format: a
    /// `PROTOCOL_ERROR`, reported as soon as its length prefix arrives.
    Nonconforming,

    /// The record is skipped without being buffered, unless its first
    /// payload byte, its kind, is this one, which makes it nonconforming.
    SkippedUnlessKind(u8),
}

/// An oversized record that is being skipped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Skipping {
    /// The record's kind byte.
    kind: u8,
    /// The payload bytes still to arrive.
    remaining: usize,
}

/// Frames record payloads on a byte stream.
///
/// Payloads are yielded as [`Bytes`] and never inspected. The decoder never
/// reserves buffer space from a length prefix, so a claimed length costs
/// nothing until the bytes arrive: a peer may hold dozens of streams open
/// (at least 40 under the draft's minimum stream limits, and more where a
/// node allows them), so reserving 2 MiB per prefix would let it pin tens or
/// hundreds of MiB with a few hundred bytes. The read buffer grows as a large
/// record arrives.
///
/// # Security
///
/// A decoder buffers an incomplete record until the rest of it arrives, so
/// each stream is framed with the longest payload its records can have, not
/// the 2 MiB every record may have: a peer can open several announcement
/// streams, and a transaction announcement is 65 bytes. A longer record
/// costs nothing: it is rejected from its length prefix, or skipped as its
/// bytes arrive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordCodec {
    /// The longest payload of a record this decoder yields.
    max_payload_len: usize,
    /// What happens to a longer record.
    oversized: Oversized,
    /// The oversized record that is being skipped, if any.
    skipping: Option<Skipping>,
}

impl Default for RecordCodec {
    /// A decoder for records of any length up to [`MAX_RECORD_PAYLOAD_LEN`].
    fn default() -> Self {
        Self::with_max_payload_len(MAX_RECORD_PAYLOAD_LEN)
    }
}

impl RecordCodec {
    /// A decoder for a stream whose records have payloads of at most
    /// `max_payload_len` bytes: a longer record does not conform to the
    /// stream's format, which is a `PROTOCOL_ERROR`, or a `FLOOD` above
    /// [`MAX_RECORD_PAYLOAD_LEN`] (draft, "Records", "Announcement Streams").
    pub fn with_max_payload_len(max_payload_len: usize) -> Self {
        RecordCodec {
            max_payload_len: max_payload_len.min(MAX_RECORD_PAYLOAD_LEN),
            oversized: Oversized::Nonconforming,
            skipping: None,
        }
    }

    /// A decoder for a stream whose records start with a kind byte, where
    /// records of kind `known_kind` have payloads of at most
    /// `max_payload_len` bytes and records of other kinds are ignored.
    ///
    /// A longer record of another kind is skipped as its bytes arrive, and
    /// then yielded as its kind byte alone, so the reader still sees one
    /// record of that kind. A longer record of `known_kind` is a
    /// `PROTOCOL_ERROR`, and any record above [`MAX_RECORD_PAYLOAD_LEN`] is a
    /// `FLOOD`.
    pub fn skipping_unknown_kinds(max_payload_len: usize, known_kind: u8) -> Self {
        RecordCodec {
            oversized: Oversized::SkippedUnlessKind(known_kind),
            ..Self::with_max_payload_len(max_payload_len)
        }
    }

    /// Returns `true` while this decoder is part way through a record it is
    /// skipping, which leaves nothing in the read buffer.
    pub fn is_skipping(&self) -> bool {
        self.skipping.is_some()
    }

    /// Encodes `payload` as one record.
    ///
    /// A payload above [`MAX_RECORD_PAYLOAD_LEN`] is a local error: callers
    /// bound their payloads before framing them.
    pub fn encode_record(payload: impl AsRef<[u8]>) -> Result<Bytes, WireError> {
        let payload = payload.as_ref();
        if payload.len() > MAX_RECORD_PAYLOAD_LEN {
            return Err(WireError::LocalCountTooLarge {
                field: "record payload length",
                count: payload.len(),
                max: MAX_RECORD_PAYLOAD_LEN,
            });
        }
        let mut record = BytesMut::with_capacity(RECORD_LENGTH_PREFIX_LEN + payload.len());
        // `payload.len()` is at most `MAX_RECORD_PAYLOAD_LEN`, which fits in a `u32`, so the cast
        // is lossless.
        record.put_u32_le(payload.len() as u32);
        record.put_slice(payload);
        Ok(record.freeze())
    }
}

impl Decoder for RecordCodec {
    type Item = Bytes;
    type Error = WireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, WireError> {
        if let Some(Skipping { kind, remaining }) = self.skipping {
            let skipped = remaining.min(src.len());
            src.advance(skipped);
            let remaining = remaining - skipped;
            if remaining > 0 {
                self.skipping = Some(Skipping { kind, remaining });
                return Ok(None);
            }
            self.skipping = None;
            return Ok(Some(Bytes::copy_from_slice(&[kind])));
        }

        if src.len() < RECORD_LENGTH_PREFIX_LEN {
            return Ok(None);
        }

        let mut prefix = &src[..RECORD_LENGTH_PREFIX_LEN];
        let length = prefix.get_u32_le();

        // The limit fits in a `u32`, so comparing as `u64` is lossless on every target.
        if u64::from(length) > MAX_RECORD_PAYLOAD_LEN as u64 {
            return Err(WireError::Flood {
                field: "record length",
                count: u64::from(length),
                max: MAX_RECORD_PAYLOAD_LEN,
            });
        }
        // `length` is at most `MAX_RECORD_PAYLOAD_LEN`, which is a `usize`, so the cast is lossless.
        let length = length as usize;

        if length > self.max_payload_len {
            let nonconforming = WireError::Nonconforming {
                what: "record",
                reason: "longer than any record of its kind",
            };
            let Oversized::SkippedUnlessKind(known_kind) = self.oversized else {
                return Err(nonconforming);
            };
            // An oversized record is longer than `max_payload_len`, so it has a kind byte.
            let Some(&kind) = src.get(RECORD_LENGTH_PREFIX_LEN) else {
                return Ok(None);
            };
            if kind == known_kind {
                return Err(nonconforming);
            }
            src.advance(RECORD_LENGTH_PREFIX_LEN + 1);
            self.skipping = Some(Skipping {
                kind,
                remaining: length - 1,
            });
            return self.decode(src);
        }

        if src.len() < RECORD_LENGTH_PREFIX_LEN + length {
            return Ok(None);
        }

        src.advance(RECORD_LENGTH_PREFIX_LEN);
        Ok(Some(src.split_to(length).freeze()))
    }

    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, WireError> {
        if let Some(record) = self.decode(src)? {
            return Ok(Some(record));
        }
        if src.is_empty() && self.skipping.is_none() {
            Ok(None)
        } else {
            Err(WireError::Truncated { what: "record" })
        }
    }
}
