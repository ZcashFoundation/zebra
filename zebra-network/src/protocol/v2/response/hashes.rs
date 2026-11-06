//! The `get-hashes` response (draft, "`get-hashes`"): `CompactSize n`, then
//! `n` [`SyncHashEntry`]s, encoded by
//! [`write_sync_hashes_body`](zebra_chain::block::sync_metadata::write_sync_hashes_body).

// The decoder is unused until the `get-hashes` client calls it.
#![allow(dead_code)]

use bytes::Bytes;

use zebra_chain::block::{
    sync_metadata::{read_sync_hash_entries, write_sync_hashes_body},
    SyncHashEntry,
};

use crate::protocol::v2::{
    constants::MAX_GET_HASHES_COUNT,
    request::GetHashes,
    wire::{ensure_consumed, read_count, WireError},
};

/// The longest conforming `get-hashes` response body: a 5-byte count and
/// `MAX_GET_HASHES_COUNT` entries of 32 + 4 × 9 bytes.
// The count is a `u32`, which fits in a `usize` on every supported target.
pub const MAX_HASHES_RESPONSE_LEN: usize =
    5 + crate::protocol::v2::constants::MAX_GET_HASHES_COUNT as usize * (32 + 4 * 9);

/// Encodes a complete response body.
///
/// # Errors
///
/// [`WireError::LocalCountTooLarge`] above `MAX_GET_HASHES_COUNT` entries.
pub fn encode_hashes_response(entries: &[SyncHashEntry]) -> Result<Bytes, WireError> {
    // A `u32` fits in a `usize` on every supported target.
    let max = MAX_GET_HASHES_COUNT as usize;
    if entries.len() > max {
        return Err(WireError::LocalCountTooLarge {
            field: "count",
            count: entries.len(),
            max,
        });
    }

    let mut body = Vec::new();
    write_sync_hashes_body(entries, &mut body)?;
    Ok(body.into())
}

/// Decodes a complete response body to `request`.
///
/// # Errors
///
/// All are connection errors (`PROTOCOL_ERROR`): more entries than
/// `request.count`, a non-canonical CompactSize, truncation, trailing bytes.
/// The prefix rule (entries start at `request.start_height`) holds by
/// construction: entry `k` is height `start_height + k`.
pub fn decode_hashes_response(
    mut body: &[u8],
    request: &GetHashes,
) -> Result<Vec<SyncHashEntry>, WireError> {
    // A `u32` fits in a `usize` on every supported target.
    let count = read_count(&mut body, "count", request.count as usize, count_too_large)?;

    // The same entry loop as `read_sync_hashes_body`, so the wire and the
    // known-hash pins parse the same canonical bytes.
    // `usize` fits in `u64` on every supported target.
    let entries = read_sync_hash_entries(&mut body, count as u64)
        .map_err(WireError::malformed("get-hashes entry"))?;
    ensure_consumed(body, "get-hashes response")?;

    Ok(entries)
}

/// More entries than requested: a nonconforming response, which closes the
/// connection with `PROTOCOL_ERROR`.
fn count_too_large(field: &'static str, count: u64, max: usize) -> WireError {
    WireError::CountTooLarge { field, count, max }
}
