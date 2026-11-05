//! The `get-block-range` request (draft, "`get-block-range`").

use std::io::Write;

use zebra_chain::{block, serialization::ZcashSerialize};

use crate::protocol::v2::{
    constants::{
        GET_BLOCK_RANGE_REQUEST_LEN, MAX_GET_BLOCK_RANGE_BYTES, MAX_GET_BLOCK_RANGE_COUNT,
    },
    wire::WireError,
};

/// A `get-block-range` request: up to `count` blocks, at most `max_bytes` of
/// them after the first, walking down the ancestor chain from `final_hash`.
///
/// The body is `final_hash[32] ‖ count u32 LE ‖ max_bytes u32 LE`
/// ([`GET_BLOCK_RANGE_REQUEST_LEN`] bytes).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GetBlockRange {
    /// The hash of the highest block of the range: the first one delivered.
    pub final_hash: block::Hash,

    /// The most blocks wanted; at most [`MAX_GET_BLOCK_RANGE_COUNT`].
    pub count: u32,

    /// The most serialized block bytes wanted, the first block exempt; at most
    /// [`MAX_GET_BLOCK_RANGE_BYTES`].
    pub max_bytes: u32,
}

impl GetBlockRange {
    /// Checks the request against the draft's bounds before sending it: a
    /// `count` or `max_bytes` above them is a local error
    /// ([`WireError::LocalCountTooLarge`]), never sent.
    pub fn check(&self) -> Result<(), WireError> {
        for (field, value, max) in self.bounded_fields() {
            if value > max {
                return Err(WireError::LocalCountTooLarge {
                    field,
                    // `u32` fits in a `usize` on every supported target, so both casts are lossless.
                    count: value as usize,
                    max: max as usize,
                });
            }
        }
        Ok(())
    }

    /// Writes the request body, after [`GetBlockRange::check`].
    pub fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        self.check()?;
        self.final_hash.zcash_serialize(&mut writer)?;
        writer.write_all(&self.count.to_le_bytes())?;
        writer.write_all(&self.max_bytes.to_le_bytes())?;
        Ok(())
    }

    /// Reads a request body. A `count` or `max_bytes` above the draft's
    /// bounds is [`WireError::CountTooLarge`] (`PROTOCOL_ERROR`), and a short
    /// body is [`WireError::Truncated`].
    pub fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        const WHAT: &str = "get-block-range request";

        // Every field is fixed-width, so the body is short exactly when it is under this length.
        let (body, rest) = reader
            .split_first_chunk::<GET_BLOCK_RANGE_REQUEST_LEN>()
            .ok_or(WireError::Truncated { what: WHAT })?;
        *reader = rest;

        // The array pattern is checked at compile time against GET_BLOCK_RANGE_REQUEST_LEN.
        let [final_hash @ .., c0, c1, c2, c3, m0, m1, m2, m3] = *body;
        let request = GetBlockRange {
            final_hash: block::Hash(final_hash),
            count: u32::from_le_bytes([c0, c1, c2, c3]),
            max_bytes: u32::from_le_bytes([m0, m1, m2, m3]),
        };

        for (field, value, max) in request.bounded_fields() {
            if value > max {
                return Err(WireError::CountTooLarge {
                    field,
                    count: value.into(),
                    // `u32` fits in a `usize` on every supported target, so the cast is lossless.
                    max: max as usize,
                });
            }
        }
        Ok(request)
    }

    /// The bounded fields with their names and the draft's bounds.
    fn bounded_fields(&self) -> [(&'static str, u32, u32); 2] {
        [
            ("count", self.count, MAX_GET_BLOCK_RANGE_COUNT),
            ("max_bytes", self.max_bytes, MAX_GET_BLOCK_RANGE_BYTES),
        ]
    }
}
