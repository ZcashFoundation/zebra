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
    amount::NonNegative,
    block::{Height, MAX_BLOCK_BYTES},
    block_info::BlockInfo,
    history_tree::{HistoryTreeError, NonEmptyHistoryTree},
    parameters::{Network, NetworkKind},
    primitives::zcash_history,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

impl IntoDisk for ValueBalance<NonNegative> {
    type Bytes = [u8; 48];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
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
        self.value_pools()
            .as_bytes()
            .iter()
            .copied()
            .chain(self.size().to_le_bytes().iter().copied())
            .collect()
    }
}

impl FromDisk for BlockInfo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        // Records are exactly 52 bytes from NU6.3 onward (48-byte value pool incl. the ironwood
        // pool, plus the 4-byte block size) and exactly 44 bytes for records written by earlier
        // Zebra versions (40-byte value pool plus 4-byte size). We discriminate the two layouts by
        // length, and stay forward-compatible by reading the known prefix
        // and ignoring any unexpected trailing bytes.
        match bytes.as_ref().len() {
            // NU6.3 onward (and any forward-compatible larger record): 48-byte pool + 4-byte size.
            52.. => {
                let value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes.as_ref()[0..48])
                    .expect("must work for 48 bytes");
                let size =
                    u32::from_le_bytes(bytes.as_ref()[48..52].try_into().expect("must be 4 bytes"));
                BlockInfo::new(value_pools, size)
            }
            // Pre-NU6.3 records (exactly 44 bytes; the open range stays forward-compatible, and the
            // 52.. arm above already took every NU6.3 record).
            44.. => {
                let value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes.as_ref()[0..40])
                    .expect("must work for 40 bytes");
                let size =
                    u32::from_le_bytes(bytes.as_ref()[40..44].try_into().expect("must be 4 bytes"));
                BlockInfo::new(value_pools, size)
            }
            _ => panic!("invalid format"),
        }
    }
}

/// The unit of a block's *size value*: the maximum serialized block size
/// divided by 255 and rounded up, so a block's size value is a 1-byte
/// quantity, and `size value × unit` is always an upper bound on the
/// block's serialized size.
///
/// The size value is a network protocol encoding, so this index stores each
/// block's raw serialized size, and the quantization is applied when the
/// metadata is served.
pub const BLOCK_SIZE_VALUE_UNIT: u64 = MAX_BLOCK_BYTES.div_ceil(255);

/// Per-block synchronization metadata, served in the v2 network protocol's
/// `get-hashes` span aggregates and `get-tree-roots` entries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SyncMetadata {
    /// The block's serialized size, in bytes.
    pub size: u32,

    /// The number of transactions in the block, including the coinbase.
    pub tx_count: u32,

    /// The number of note commitments added by the block: two per JoinSplit
    /// description, one per Sapling output description, and one per Orchard
    /// or Ironwood action description.
    pub note_count: u32,

    /// The number of transactions with Sapling components, as counted by the
    /// block's ZIP 221 chain history tree leaf.
    pub sapling_tx_count: u32,

    /// The number of transactions with Orchard-pool components.
    pub orchard_tx_count: u32,

    /// The number of transactions with Ironwood-pool components.
    pub ironwood_tx_count: u32,

    /// The block's ZIP 244 authorizing data commitment, `hashAuthDataRoot`.
    pub auth_data_root: [u8; 32],

    /// The number of transparent outputs created by this block and every block
    /// below it in the chain.
    ///
    /// This count assigns each transparent output a global ordinal: the parent
    /// block's cumulative count, plus the output's index among this block's
    /// outputs in transaction order. Genesis outputs count toward the ordinal:
    /// the SwiftSync spentness bitmap indexes every transparent output created
    /// at or below its checkpoint in canonical order, and the genesis coinbase
    /// creates outputs even though Zebra never indexes them as spendable.
    pub cumulative_transparent_outputs: u64,
}

impl SyncMetadata {
    /// Computes the synchronization metadata of `block`, whose serialized
    /// size is `serialized_size`, and whose parent block's cumulative
    /// transparent output count is `prev_cumulative_transparent_outputs`
    /// (zero for the genesis block).
    pub fn for_block(
        block: &zebra_chain::block::Block,
        serialized_size: usize,
        prev_cumulative_transparent_outputs: u64,
    ) -> SyncMetadata {
        // Counted through the block's own note commitment iterators, so
        // this count follows the pools zebra-chain knows about.
        let note_count = block.sprout_note_commitments().count()
            + block.sapling_note_commitments().count()
            + block.orchard_note_commitments().count()
            + block.ironwood_note_commitments().count();

        let transparent_output_count: usize =
            block.transactions.iter().map(|tx| tx.outputs().len()).sum();

        let cumulative_transparent_outputs = prev_cumulative_transparent_outputs
            // The cast is safe: the per-block count is bounded by
            // `MAX_BLOCK_BYTES` over the minimum output encoding, far below
            // u64. The sum can't overflow, because every counted output
            // takes tens of bytes in a chain whose length fits in u32.
            .checked_add(transparent_output_count as u64)
            .expect(
                "a u32-height chain of MAX_BLOCK_BYTES blocks of minimal outputs stays below u64::MAX outputs",
            );

        SyncMetadata {
            // The casts below are safe: consensus-valid blocks are at most
            // `MAX_BLOCK_BYTES`, and each count is bounded by that size
            // over the minimum encoding of a counted item, far below u32.
            size: serialized_size as u32,
            tx_count: block.transactions.len() as u32,
            note_count: note_count as u32,
            sapling_tx_count: block.sapling_transactions_count() as u32,
            orchard_tx_count: block.orchard_transactions_count() as u32,
            ironwood_tx_count: block.ironwood_transactions_count() as u32,
            auth_data_root: block.auth_data_root().into(),
            cumulative_transparent_outputs,
        }
    }
}

impl IntoDisk for SyncMetadata {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&self.size.to_le_bytes());
        bytes.extend_from_slice(&self.tx_count.to_le_bytes());
        bytes.extend_from_slice(&self.note_count.to_le_bytes());
        bytes.extend_from_slice(&self.sapling_tx_count.to_le_bytes());
        bytes.extend_from_slice(&self.orchard_tx_count.to_le_bytes());
        bytes.extend_from_slice(&self.ironwood_tx_count.to_le_bytes());
        bytes.extend_from_slice(&self.auth_data_root);
        bytes.extend_from_slice(&self.cumulative_transparent_outputs.to_le_bytes());
        bytes
    }
}

impl FromDisk for SyncMetadata {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        // Records are exactly 64 bytes; reading the known prefix and
        // ignoring unexpected trailing bytes stays forward-compatible.
        assert!(bytes.len() >= 64, "invalid format");

        let word =
            |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("must be 4 bytes"));

        SyncMetadata {
            size: word(0),
            tx_count: word(4),
            note_count: word(8),
            sapling_tx_count: word(12),
            orchard_tx_count: word(16),
            ironwood_tx_count: word(20),
            auth_data_root: bytes[24..56].try_into().expect("must be 32 bytes"),
            cumulative_transparent_outputs: u64::from_le_bytes(
                bytes[56..64].try_into().expect("must be 8 bytes"),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
