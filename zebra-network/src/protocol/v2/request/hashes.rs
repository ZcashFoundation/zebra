//! The `get-hashes` request (draft, "`get-hashes`").

use std::io::Write;

use zebra_chain::block;

use crate::protocol::v2::{
    constants::{GET_HASHES_REQUEST_LEN, MAX_GET_HASHES_COUNT},
    wire::WireError,
};

/// A `get-hashes` request: the best-chain entries for `count` heights from
/// `start_height`.
///
/// The body is `start_height u32 LE ‖ count u32 LE`
/// ([`GET_HASHES_REQUEST_LEN`](crate::protocol::v2::constants::GET_HASHES_REQUEST_LEN)
/// bytes). The stride variant of #11349 is not supported.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GetHashes {
    /// The height of the first entry.
    pub start_height: block::Height,

    /// The most entries wanted; at most
    /// [`MAX_GET_HASHES_COUNT`](crate::protocol::v2::constants::MAX_GET_HASHES_COUNT),
    /// and `start_height + count - 1` at most `u32::MAX`.
    pub count: u32,
}

impl GetHashes {
    /// Checks the request against the draft's bounds before sending it.
    ///
    /// A `count` above [`MAX_GET_HASHES_COUNT`] is
    /// [`WireError::LocalCountTooLarge`], and a greatest requested height
    /// above `u32::MAX` is [`WireError::LocalUnencodable`]; neither is sent.
    pub fn check(&self) -> Result<(), WireError> {
        if self.count > MAX_GET_HASHES_COUNT {
            return Err(WireError::LocalCountTooLarge {
                field: "count",
                // A `u32` fits in a `usize` on every supported target.
                count: self.count as usize,
                max: MAX_GET_HASHES_COUNT as usize,
            });
        }
        if self.greatest_height_overflows() {
            return Err(WireError::LocalUnencodable {
                what: "get-hashes request",
                reason: "the greatest requested height exceeds u32::MAX",
            });
        }
        Ok(())
    }

    /// Writes the request body, after [`GetHashes::check`].
    pub fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        self.check()?;
        writer.write_all(&self.start_height.0.to_le_bytes())?;
        writer.write_all(&self.count.to_le_bytes())?;
        Ok(())
    }

    /// Reads a request body; a bound violation is `PROTOCOL_ERROR`.
    ///
    /// A `count` above [`MAX_GET_HASHES_COUNT`], or one that puts the greatest
    /// requested height above `u32::MAX`, is [`WireError::CountTooLarge`]; a
    /// short body is [`WireError::Truncated`].
    pub fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        if reader.len() < GET_HASHES_REQUEST_LEN {
            return Err(WireError::Truncated {
                what: "get-hashes request",
            });
        }
        let start_height = read_u32_le(reader);
        let count = read_u32_le(reader);

        let request = GetHashes {
            start_height: block::Height(start_height),
            count,
        };

        if count > MAX_GET_HASHES_COUNT {
            return Err(WireError::CountTooLarge {
                field: "count",
                count: count.into(),
                // A `u32` fits in a `usize` on every supported target.
                max: MAX_GET_HASHES_COUNT as usize,
            });
        }
        if request.greatest_height_overflows() {
            return Err(WireError::CountTooLarge {
                field: "count",
                count: count.into(),
                // The height overflows, so `u32::MAX - start_height` is below `count`, which
                // is at most `MAX_GET_HASHES_COUNT`: the cast is lossless.
                max: (u32::MAX - start_height) as usize + 1,
            });
        }

        Ok(request)
    }

    /// Whether `start_height + count - 1` exceeds `u32::MAX`. An empty request
    /// asks for no heights, so it never does.
    fn greatest_height_overflows(&self) -> bool {
        self.count
            .checked_sub(1)
            .is_some_and(|last| self.start_height.0.checked_add(last).is_none())
    }
}

/// Reads a little-endian `u32` from a `reader` the caller has checked holds at
/// least four bytes.
fn read_u32_le(reader: &mut &[u8]) -> u32 {
    let (bytes, rest) = reader
        .split_first_chunk::<4>()
        .expect("the caller checked the body length");
    *reader = rest;
    u32::from_le_bytes(*bytes)
}
