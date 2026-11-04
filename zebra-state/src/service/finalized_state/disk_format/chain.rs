//! Chain data serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::collections::BTreeMap;

use ::zcash_history::{NodeData, NodeDataV2, NodeDataV3, MAX_ENTRY_SIZE};
use bincode::Options;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, merkle::AuthDataRoot, Height, SyncHashEntry},
    block_info::BlockInfo,
    history_tree::{HistoryTreeError, NonEmptyHistoryTree},
    parameters::{Network, NetworkKind, NetworkUpgrade},
    primitives::zcash_history,
    state_snapshot::ObjectHash,
    value_balance::{ValueBalance, SERIALIZED_SIZE},
};

use crate::{
    service::finalized_state::disk_format::{block::height_from_bytes_prefix, FromDisk, IntoDisk},
    state_snapshot::SnapshotBase,
};

impl SnapshotBase {
    /// Decodes a snapshot marker without panicking on malformed database bytes.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let (window_start, rest) = height_from_bytes_prefix(bytes)
            .ok_or("the snapshot base has an invalid window start")?;
        let (height, rest) =
            height_from_bytes_prefix(rest).ok_or("the snapshot base has an invalid height")?;
        let (block_hash, rest) = rest
            .split_at_checked(32)
            .ok_or("the snapshot base has no block hash")?;
        let (manifest, rest) = rest
            .split_at_checked(32)
            .ok_or("the snapshot base has no manifest hash")?;
        let (&deferred_checks, rest) = rest
            .split_first()
            .ok_or("the snapshot base has no deferred checks")?;
        if !rest.is_empty() {
            return Err("the snapshot base has trailing bytes");
        }

        Ok(SnapshotBase {
            window_start,
            height,
            block_hash: block::Hash(
                block_hash
                    .try_into()
                    .expect("32 block hash bytes were sliced"),
            ),
            manifest: ObjectHash(
                manifest
                    .try_into()
                    .expect("32 manifest hash bytes were sliced"),
            ),
            deferred_checks,
        })
    }
}

impl IntoDisk for SnapshotBase {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        [
            self.window_start.as_bytes(),
            self.height.as_bytes(),
            self.block_hash.as_bytes().to_vec(),
            self.manifest.0.to_vec(),
            vec![self.deferred_checks],
        ]
        .concat()
    }
}

impl FromDisk for SnapshotBase {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::decode(bytes.as_ref())
            .expect("stored snapshot bases must use their canonical encoding")
    }
}

impl IntoDisk for ValueBalance<NonNegative> {
    type Bytes = [u8; SERIALIZED_SIZE];

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
    /// Decodes a state snapshot's `history_tree` value for `network` without trusting it: the
    /// bytes must be exactly the current encoding of a tree for this network at a height where
    /// the history tree exists, and the tree must rebuild from its peaks.
    pub(crate) fn decode_canonical(
        bytes: &[u8],
        network: &Network,
    ) -> Result<NonEmptyHistoryTree, String> {
        let parts: HistoryTreeParts = bincode::DefaultOptions::new()
            .deserialize(bytes)
            .map_err(|error| format!("the history tree does not decode: {error}"))?;

        if parts.as_bytes() != bytes {
            return Err("the history tree is not in the canonical encoding".to_string());
        }

        if parts.network_kind != network.kind() {
            return Err(format!(
                "the history tree is for {:?}, not {:?}",
                parts.network_kind,
                network.kind()
            ));
        }

        let upgrade = NetworkUpgrade::current(network, parts.current_height);
        if upgrade < NetworkUpgrade::Heartwood {
            return Err(format!(
                "the history tree ends at height {}, before Heartwood",
                parts.current_height.0
            ));
        }

        check_history_tree_peaks(bytes, upgrade)?;
        parts
            .with_network(network)
            .map_err(|error| format!("the history tree does not rebuild from its peaks: {error}"))
    }

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

    /// Returns the kind of network the tree is for.
    pub(crate) fn network_kind(&self) -> NetworkKind {
        self.network_kind
    }

    /// Returns the height of the last block in the tree.
    pub(crate) fn current_height(&self) -> Height {
        self.current_height
    }

    /// Returns the number of peaks in the tree, which is never zero for a tree
    /// that [`Self::with_network`] can rebuild.
    pub(crate) fn peak_count(&self) -> usize {
        self.peaks.len()
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

/// A stored history tree, with its peaks as bytes: the layout of
/// [`HistoryTreeParts`], whose fields are private.
#[derive(Serialize, Deserialize)]
pub(crate) struct RawHistoryTree {
    network_kind: NetworkKind,
    size: u32,
    pub(crate) peaks: BTreeMap<u32, RawEntry>,
    current_height: Height,
}

/// A stored history tree entry: its kind, then the node data of the tree's version.
#[derive(Serialize, Deserialize)]
pub(crate) struct RawEntry {
    #[serde(with = "BigArray")]
    pub(crate) inner: [u8; MAX_ENTRY_SIZE],
}

/// Checks that the history tree `bytes` rebuilds without panicking: it has peaks, every peak is an
/// entry of the tree version of `upgrade`, and the work and transaction counts added while combining
/// them do not overflow.
pub(crate) fn check_history_tree_peaks(
    bytes: &[u8],
    upgrade: NetworkUpgrade,
) -> Result<(), String> {
    let raw: RawHistoryTree = bincode::DefaultOptions::new()
        .deserialize(bytes)
        .map_err(|error| format!("invalid history tree: {error}"))?;
    let branch_id = upgrade
        .branch_id()
        .ok_or_else(|| format!("{upgrade:?} has no history tree"))?;
    let overflow = |what| format!("the history tree's {what} overflow when its peaks are combined");

    let mut peaks = raw
        .peaks
        .values()
        .map(|entry| read_peak(upgrade, branch_id.into(), &entry.inner));
    let (first, [orchard, ironwood]) = peaks
        .next()
        .ok_or_else(|| "the history tree has no peaks".to_string())??;
    let mut work = first.subtree_total_work;
    let mut transactions = [first.sapling_tx, orchard, ironwood];
    for peak in peaks {
        let (data, [orchard, ironwood]) = peak?;
        work = work
            .checked_add(data.subtree_total_work)
            .ok_or_else(|| overflow("total work"))?;
        for (total, count) in transactions
            .iter_mut()
            .zip([data.sapling_tx, orchard, ironwood])
        {
            *total = total
                .checked_add(count)
                .ok_or_else(|| overflow("transaction counts"))?;
        }
    }
    Ok(())
}

/// Parses a stored history tree peak as an entry of the tree version of `upgrade`, and returns its
/// node data before Orchard, with its Orchard and Ironwood transaction counts.
fn read_peak(
    upgrade: NetworkUpgrade,
    branch_id: u32,
    entry: &[u8],
) -> Result<(NodeData, [u64; 2]), String> {
    use NetworkUpgrade::*;

    // An entry is its kind, then its node data: a leaf, or a node with the indexes of its two children.
    let header = match entry.first().copied() {
        Some(1) => 1,
        Some(0) => 1 + 2 * 4,
        _ => return Err("a history tree peak has an unknown entry kind".to_string()),
    };
    let mut reader = entry
        .get(header..)
        .ok_or_else(|| "a history tree peak is truncated".to_string())?;
    let invalid = |error| format!("invalid history tree peak: {error}");
    Ok(match upgrade {
        Genesis | BeforeOverwinter | Overwinter | Sapling | Blossom => {
            return Err(format!("{upgrade:?} has no history tree"))
        }
        Heartwood | Canopy => (
            NodeData::read(branch_id, &mut reader).map_err(invalid)?,
            [0, 0],
        ),
        Nu5 | Nu6 | Nu6_1 | Nu6_2 => {
            let data = NodeDataV2::read(branch_id, &mut reader).map_err(invalid)?;
            (data.v1, [data.orchard_tx, 0])
        }
        // NU6.3 and every later upgrade use V3 trees, as `NonEmptyHistoryTree::from_cache` does.
        _ => {
            let data = NodeDataV3::read(branch_id, &mut reader).map_err(invalid)?;
            (data.v2.v1, [data.v2.orchard_tx, data.ironwood_tx])
        }
    })
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
pub(crate) struct LegacyHistoryTreeParts {
    network_kind: NetworkKind,
    size: u32,
    peaks: BTreeMap<u32, LegacyEntry>,
    current_height: Height,
}

/// A history-tree entry serialized at the pre-NU6.3 [`LEGACY_MAX_ENTRY_SIZE`] width.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct LegacyEntry {
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
    type Bytes = [u8; SERIALIZED_SIZE + 4];

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = [0; SERIALIZED_SIZE + 4];
        let value_pools = self.value_pools().to_bytes();
        bytes[..48].copy_from_slice(&value_pools[..48]);
        bytes[48..52].copy_from_slice(&self.size().to_le_bytes());
        bytes[52..].copy_from_slice(&value_pools[48..]);
        bytes
    }
}

impl FromDisk for BlockInfo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        // Preserve the v28 pool-and-size prefix; append NSM after the block size.
        // Legacy layouts leave newer pools zero; ignore forward-compatible trailing bytes.
        match bytes.len() {
            60.. => {
                let mut value_pools = ValueBalance::<NonNegative>::from_bytes(&bytes[..48])
                    .expect("must work for 48 bytes");
                let size = u32::from_le_bytes(bytes[48..52].try_into().expect("must be 4 bytes"));
                let nsm = Amount::from_bytes(bytes[52..60].try_into().expect("must be 8 bytes"))
                    .expect("stored NSM balance must be a valid nonnegative amount");
                value_pools.set_nsm_amount(nsm);
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

/// Per-block synchronization metadata: the 64-byte value of the
/// `sync_meta_by_height` column family (format 29.1.0).
///
/// Layout, little-endian: `size u32 | txs u32 | notes u32 | sapling_txs u32
/// | orchard_txs u32 | ironwood_txs u32 | auth_data_root [32] | cum_txouts u64`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SyncMetadata {
    /// The block's serialized size, in bytes.
    pub size: u32,

    /// The number of transactions in the block, including the coinbase.
    pub tx_count: u32,

    /// The number of note commitments the block adds: two per JoinSplit,
    /// one per Sapling output, one per Orchard or Ironwood action.
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

    /// The number of transparent outputs created by this block and every
    /// block below it, genesis outputs included: the spentness-hint ordinal
    /// of this block's first output is the parent's value.
    ///
    /// Genesis outputs count toward the ordinal, because the spentness
    /// bitmap indexes every transparent output created at or below its
    /// checkpoint in canonical order, and the genesis coinbase creates
    /// outputs even though Zebra never indexes them as spendable.
    pub cumulative_transparent_outputs: u64,
}

impl SyncMetadata {
    /// The length of a serialized record.
    pub const SERIALIZED_LEN: usize = 64;

    /// Computes the synchronization metadata of `block`, whose serialized
    /// size is `serialized_size`, and whose parent block's cumulative
    /// transparent output count is `prev_cumulative_transparent_outputs`
    /// (zero for the genesis block).
    ///
    /// Uses `precomputed_auth_data_root` if it is provided, which must have
    /// been computed from this block body. Otherwise computes the root.
    pub fn for_block(
        block: &zebra_chain::block::Block,
        serialized_size: usize,
        prev_cumulative_transparent_outputs: u64,
        precomputed_auth_data_root: Option<AuthDataRoot>,
    ) -> SyncMetadata {
        // The size and the transaction, output and note counts are the block's `get-hashes`
        // entry, which is their only definition, so that this node serves exactly the bytes the
        // known-hash pins were computed from.
        let entry = SyncHashEntry::for_block(block, serialized_size);

        let cumulative_transparent_outputs = prev_cumulative_transparent_outputs
            // The sum can't overflow, because every counted output takes tens of bytes in a chain
            // whose length fits in u32.
            .checked_add(entry.txouts)
            .expect(
                "a u32-height chain of MAX_BLOCK_BYTES blocks of minimal outputs stays below u64::MAX outputs",
            );

        SyncMetadata {
            // The casts below are safe: consensus-valid blocks are at most
            // `MAX_BLOCK_BYTES`, and each count is bounded by that size
            // over the minimum encoding of a counted item, far below u32.
            size: entry.size as u32,
            tx_count: entry.txs as u32,
            note_count: entry.notes as u32,
            sapling_tx_count: block.sapling_transactions_count() as u32,
            orchard_tx_count: block.orchard_transactions_count() as u32,
            ironwood_tx_count: block.ironwood_transactions_count() as u32,
            auth_data_root: precomputed_auth_data_root
                .unwrap_or_else(|| block.auth_data_root())
                .into(),
            cumulative_transparent_outputs,
        }
    }
}

impl IntoDisk for SyncMetadata {
    type Bytes = [u8; SyncMetadata::SERIALIZED_LEN];

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = [0; SyncMetadata::SERIALIZED_LEN];
        bytes[0..4].copy_from_slice(&self.size.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.tx_count.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.note_count.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.sapling_tx_count.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.orchard_tx_count.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.ironwood_tx_count.to_le_bytes());
        bytes[24..56].copy_from_slice(&self.auth_data_root);
        bytes[56..64].copy_from_slice(&self.cumulative_transparent_outputs.to_le_bytes());
        bytes
    }
}

impl FromDisk for SyncMetadata {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        // Records are exactly 64 bytes; reading the known prefix and
        // ignoring unexpected trailing bytes stays forward-compatible.
        assert!(
            bytes.len() >= SyncMetadata::SERIALIZED_LEN,
            "invalid format"
        );

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
