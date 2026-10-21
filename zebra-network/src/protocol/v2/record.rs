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

/// Frames record payloads on a byte stream.
///
/// Payloads are yielded as [`Bytes`] and never inspected. The decoder never
/// reserves buffer space from a length prefix, so a claimed length costs
/// nothing until the bytes arrive: a peer may hold dozens of streams open
/// (at least 40 under the draft's minimum stream limits, and more where a
/// node allows them), so reserving 2 MiB per prefix would let it pin tens or
/// hundreds of MiB with a few hundred bytes. The read buffer grows as a large
/// record arrives.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecordCodec;

impl RecordCodec {
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
        if src.is_empty() {
            Ok(None)
        } else {
            Err(WireError::Truncated { what: "record" })
        }
    }
}
