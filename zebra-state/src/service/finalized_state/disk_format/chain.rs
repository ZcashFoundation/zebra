//! Chain data serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::collections::BTreeMap;

use bincode::Options;
use serde_big_array::BigArray;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    block_info::BlockInfo,
    history_tree::{HistoryTreeError, NonEmptyHistoryTree},
    parameters::{Network, NetworkKind},
    primitives::zcash_history,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

/// The width of a serialized [`ValueBalance<NonNegative>`] before NU7 added the `nsm_reserve`
/// balance: six pool balances at 8 bytes each.
const V28_VALUE_POOL_BYTES: usize = 48;

impl IntoDisk for ValueBalance<NonNegative> {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let bytes = self.to_bytes();

        // The `nsm_reserve` balance is the last 8 bytes of the 56-byte form, so truncating it
        // yields the v28.0 record exactly. Height-aware writers omit the derived seed before
        // NU7, and zero reserves continue to use the narrow layout afterwards.
        if self.nsm_reserve_amount().is_zero() {
            bytes[..V28_VALUE_POOL_BYTES].to_vec()
        } else {
            bytes.to_vec()
        }
    }
}

impl FromDisk for ValueBalance<NonNegative> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        ValueBalance::from_bytes(bytes.as_ref()).expect("ValueBalance should be parsable")
    }
}

// The following implementations for history trees use `serde` and
// `bincode`. `serde` serializations depend on the inner structure of the type.
// They should not be used in new code. (This is an issue for any derived serialization format.)
//
// We explicitly use `bincode::DefaultOptions`  to disallow trailing bytes; see
// https://docs.rs/bincode/1.3.3/bincode/config/index.html#options-struct-vs-bincode-functions

#[derive(serde::Serialize, serde::Deserialize)]
pub struct HistoryTreeParts {
    network_kind: NetworkKind,
    size: u32,
    peaks: BTreeMap<u32, zcash_history::Entry>,
    current_height: Height,
}

impl HistoryTreeParts {
    /// Converts [`HistoryTreeParts`] to a [`NonEmptyHistoryTree`].
    pub(crate) fn with_network(
        self,
        network: &Network,
    ) -> Result<NonEmptyHistoryTree, HistoryTreeError> {
        assert_eq!(
            self.network_kind,
            network.kind(),
            "history tree network kind should match current network"
        );

        NonEmptyHistoryTree::from_cache(network, self.size, self.peaks, self.current_height)
    }
}

impl From<&NonEmptyHistoryTree> for HistoryTreeParts {
    fn from(history_tree: &NonEmptyHistoryTree) -> Self {
        HistoryTreeParts {
            network_kind: history_tree.network().kind(),
            size: history_tree.size(),
            peaks: history_tree.peaks().clone(),
            current_height: history_tree.current_height(),
        }
    }
}

impl IntoDisk for HistoryTreeParts {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

/// The width of a history-tree [`zcash_history::Entry`] as serialized by database formats written
/// before NU6.3 (Ironwood) widened `zcash_history::NodeData`. Equal to the pre-Ironwood
/// `MAX_ENTRY_SIZE`: a `MAX_NODE_DATA_SIZE` of 244 bytes plus the 9-byte entry header.
const LEGACY_MAX_ENTRY_SIZE: usize = 253;

/// A mirror of [`HistoryTreeParts`] using the pre-NU6.3 [`zcash_history::Entry`] width.
///
/// Used to read history trees written by earlier database formats, whose entries are too narrow to
/// parse at the current width. The field order must match [`HistoryTreeParts`] so the two share a
/// bincode layout (bincode ignores field names and identifies fields by position).
#[derive(serde::Serialize, serde::Deserialize)]
struct LegacyHistoryTreeParts {
    network_kind: NetworkKind,
    size: u32,
    peaks: BTreeMap<u32, LegacyEntry>,
    current_height: Height,
}

/// A history-tree entry serialized at the pre-NU6.3 [`LEGACY_MAX_ENTRY_SIZE`] width.
#[derive(serde::Serialize, serde::Deserialize)]
struct LegacyEntry {
    #[serde(with = "BigArray")]
    inner: [u8; LEGACY_MAX_ENTRY_SIZE],
}

impl From<LegacyHistoryTreeParts> for HistoryTreeParts {
    fn from(legacy: LegacyHistoryTreeParts) -> Self {
        HistoryTreeParts {
            network_kind: legacy.network_kind,
            size: legacy.size,
            peaks: legacy
                .peaks
                .into_iter()
                .map(|(index, entry)| {
                    (
                        index,
                        zcash_history::Entry::from_raw_bytes_padded(&entry.inner),
                    )
                })
                .collect(),
            current_height: legacy.current_height,
        }
    }
}

impl FromDisk for HistoryTreeParts {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        let options = bincode::DefaultOptions::new();

        // Try the current entry width first. Databases written before NU6.3 (Ironwood) widened
        // `zcash_history::Entry` store narrower entries that fail to parse at the current width, so
        // fall back to the legacy width and zero-pad each entry up to the current width. New data
        // always parses at the current width, so it never reaches the fallback.
        //
        // The fallback must run on *any* current-width parse error, not just `UnexpectedEof`.
        // Entries are fixed-size arrays with no length prefix, so parsing a multi-peak legacy record
        // at the wider width reads later `BTreeMap` keys from the middle of an entry's bytes; bincode
        // then fails with a varint/integer-range error (not `UnexpectedEof`) whenever a misread key
        // byte is out of range. Gating on `UnexpectedEof` alone panicked on those records. A genuine
        // current-format record only reaches the fallback if it is corrupt, in which case the legacy
        // parse fails too and the error still surfaces.
        options
            .deserialize::<HistoryTreeParts>(bytes)
            .or_else(|_| {
                options
                    .deserialize::<LegacyHistoryTreeParts>(bytes)
                    .map(HistoryTreeParts::from)
            })
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

impl IntoDisk for BlockInfo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        // The v28.0 fields stay at their v28.0 offsets — the 48-byte value pool prefix (without
        // the NU7 NSM reserve), then the block size — and the reserve is appended after them, so
        // v28.0 code reading a wide record still reads the correct size and treats the reserve
        // as zero. The record only widens once the reserve is non-zero, which can not happen
        // before NU7 activation.
        let value_pools = self.value_pools();
        let mut bytes = value_pools.to_bytes()[..V28_VALUE_POOL_BYTES].to_vec();
        bytes.extend(self.size().to_le_bytes());
        if !value_pools.nsm_reserve_amount().is_zero() {
            bytes.extend(value_pools.nsm_reserve_amount().to_bytes());
        }
        bytes
    }
}

impl FromDisk for BlockInfo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        // Records are exactly 52 bytes while the NU7 NSM reserve is zero (the v28.0 layout: a
        // 48-byte value pool, then the 4-byte block size), exactly 60 bytes once the reserve is
        // non-zero (the reserve appended after the size, so the size stays at its v28.0 offset),
        // and exactly 44 bytes for records written before NU6.3 (a 40-byte value pool, then the
        // size). Layouts are discriminated by length, longest first, staying forward-compatible
        // by reading the known fields and ignoring any unexpected trailing bytes.
        match bytes.len() {
            // Wide records (and any forward-compatible larger record): v28.0 fields, then the
            // 8-byte NSM reserve.
            60.. => {
                let mut value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes[0..48])
                    .expect("must work for 48 bytes");
                let size = u32::from_le_bytes(bytes[48..52].try_into().expect("must be 4 bytes"));
                let nsm_reserve = Amount::<NonNegative>::from_bytes(
                    bytes[52..60].try_into().expect("must be 8 bytes"),
                )
                .expect("NSM reserve amount must be parsable");
                value_pools.set_nsm_reserve_amount(nsm_reserve);
                BlockInfo::new(value_pools, size)
            }
            // v28.0 records (exactly 52 bytes; the `60..` arm above already took every wide
            // record): 48-byte pool + 4-byte size.
            52.. => {
                let value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes[0..48])
                    .expect("must work for 48 bytes");
                let size = u32::from_le_bytes(bytes[48..52].try_into().expect("must be 4 bytes"));
                BlockInfo::new(value_pools, size)
            }
            // Pre-NU6.3 records (exactly 44 bytes; the arms above already took every NU6.3 and
            // NU7 record).
            44.. => {
                let value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes[0..40])
                    .expect("must work for 40 bytes");
                let size = u32::from_le_bytes(bytes[40..44].try_into().expect("must be 4 bytes"));
                BlockInfo::new(value_pools, size)
            }
            _ => panic!("invalid format"),
        }
    }
}

#[cfg(test)]
mod tests;
