//! The record checks of each snapshot component (design, "Snapshot
//! contents" and "Formats"), run on
//! every record before it is written to an SST file.
//!
//! A record must have the key and value encoding of the current database
//! format: a key of the component's key type, and a value the database's
//! readers decode. Canonicalized components must be in their canonical
//! encoding, and their note commitment trees must carry the roots recomputed
//! from their frontiers: a cached root is never trusted. Once a record passes,
//! the database's `FromDisk` readers can read it without panicking.
//!
//! Note commitment trees and the history tree are decoded with zebra-chain's
//! serde implementations, which reject truncated or malformed encodings but
//! panic on some well-formed ones: a frontier with the wrong number of ommers
//! for its position or a position past its depth, a node or root that is not a
//! field element, and history tree peaks whose work or transaction counts
//! overflow when the tree combines them. Zebra aborts on a panic, so the
//! importer first decodes mirrors of the stored layouts ([`RawTree`] and
//! `RawHistoryTree`) and refuses such a value.

use bincode::Options;
use serde::Deserialize;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    block_info::BlockInfo,
    orchard,
    parameters::NetworkUpgrade,
    sapling,
    serialization::ZcashDeserialize,
    sprout,
    transaction::Transaction,
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        HistoryTreeParts, OutputLocation, TransactionLocation, HEIGHT_DISK_BYTES,
        OUTPUT_LOCATION_DISK_BYTES, TRANSACTION_LOCATION_DISK_BYTES,
    },
    state_snapshot::format::ComponentId,
    IntoDisk,
};

/// The width of a hash, nullifier or note commitment tree root.
const HASH_BYTES: usize = 32;

/// The width of a note commitment subtree index.
const SUBTREE_INDEX_BYTES: usize = 2;

/// The width of a note commitment subtree: its end height and root.
const SUBTREE_BYTES: usize = HEIGHT_DISK_BYTES + HASH_BYTES;

/// The width of a chain value pool record in the current format.
const VALUE_BALANCE_BYTES: usize = zebra_chain::value_balance::SERIALIZED_SIZE;

/// The width of a block info record in the current format: the value pools
/// before the NSM pool, the block size, and the NSM pool.
const BLOCK_INFO_BYTES: usize = VALUE_BALANCE_BYTES + 4;

/// The depth of the Sprout note commitment tree, `MerkleDepth^Sprout`.
const SPROUT_MERKLE_DEPTH: u8 = 29;

/// The depth of the Sapling note commitment tree, `MerkleDepth^Sapling`.
const SAPLING_MERKLE_DEPTH: u8 = 32;

/// The depth of the Orchard and Ironwood note commitment trees,
/// `MerkleDepth^Orchard`.
const ORCHARD_MERKLE_DEPTH: u8 = 32;

/// Checks one record of `component`, and returns why it is invalid.
pub(super) fn check_record(component: ComponentId, key: &[u8], value: &[u8]) -> Result<(), String> {
    use ComponentId::*;

    match component {
        HashByHeight => {
            width(key, HEIGHT_DISK_BYTES, "height")?;
            width(value, HASH_BYTES, "block hash")
        }
        HeightByHash => {
            width(key, HASH_BYTES, "block hash")?;
            width(value, HEIGHT_DISK_BYTES, "height")
        }
        Headers => {
            width(key, HEIGHT_DISK_BYTES, "height")?;
            decode_all::<block::Header>(value, "block header").map(drop)
        }
        Transactions => {
            width(key, TRANSACTION_LOCATION_DISK_BYTES, "transaction location")?;
            decode_all::<Transaction>(value, "transaction").map(drop)
        }
        TxLocByHash => {
            width(key, HASH_BYTES, "transaction hash")?;
            width(
                value,
                TRANSACTION_LOCATION_DISK_BYTES,
                "transaction location",
            )
        }
        BlockInfo => {
            width(key, HEIGHT_DISK_BYTES, "height")?;
            block_info(value).map(drop)
        }
        Utxos => {
            width(key, OUTPUT_LOCATION_DISK_BYTES, "output location")?;
            decode_all::<transparent::Output>(value, "transparent output").map(drop)
        }
        SproutNullifiers | SaplingNullifiers | OrchardNullifiers | IronwoodNullifiers => {
            width(key, HASH_BYTES, "nullifier")?;
            width(value, 0, "nullifier value")
        }
        SproutAnchors => {
            width(key, HASH_BYTES, "Sprout anchor")?;
            let tree = sprout_tree(value)?;
            if tree.recalculate_root().as_bytes() != key {
                return Err("Sprout anchor is not the root of its tree".to_string());
            }
            Ok(())
        }
        SaplingAnchors => {
            sapling::tree::Root::try_from(hash(key, "Sapling anchor")?)
                .map_err(|_| "Sapling anchor is not a field element".to_string())?;
            width(value, 0, "Sapling anchor value")
        }
        OrchardAnchors | IronwoodAnchors => {
            orchard::tree::Root::try_from(hash(key, "anchor")?)
                .map_err(|_| "anchor is not a field element".to_string())?;
            width(value, 0, "anchor value")
        }
        SaplingTree => {
            width(key, HEIGHT_DISK_BYTES, "height")?;
            sapling_tree(value).map(drop)
        }
        OrchardTree | IronwoodTree => {
            width(key, HEIGHT_DISK_BYTES, "height")?;
            orchard_tree(value).map(drop)
        }
        SaplingSubtrees => {
            width(key, SUBTREE_INDEX_BYTES, "subtree index")?;
            width(value, SUBTREE_BYTES, "Sapling subtree")?;
            let root = hash(&value[HEIGHT_DISK_BYTES..], "Sapling subtree root")?;
            Option::<sapling_crypto::Node>::from(sapling_crypto::Node::from_bytes(root))
                .ok_or_else(|| "Sapling subtree root is not a field element".to_string())
                .map(drop)
        }
        OrchardSubtrees | IronwoodSubtrees => {
            width(key, SUBTREE_INDEX_BYTES, "subtree index")?;
            width(value, SUBTREE_BYTES, "subtree")?;
            orchard::tree::Node::try_from(&value[HEIGHT_DISK_BYTES..])
                .map_err(|_| "subtree root is not a field element".to_string())
                .map(drop)
        }
        SproutTipTree => {
            width(key, 0, "Sprout tip tree key")?;
            sprout_tree(value).map(drop)
        }
        HistoryTree => {
            width(key, 0, "history tree key")?;
            history_tree_parts(value).map(drop)
        }
        ValuePools => {
            width(key, 0, "value pool key")?;
            value_pools(value).map(drop)
        }
    }
}

/// Checks that `bytes` is `expected` bytes long.
fn width(bytes: &[u8], expected: usize, what: &str) -> Result<(), String> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(format!("{what} is {} bytes, not {expected}", bytes.len()))
    }
}

/// Returns `bytes` as a 32-byte hash or root.
fn hash(bytes: &[u8], what: &str) -> Result<[u8; HASH_BYTES], String> {
    bytes
        .try_into()
        .map_err(|_| format!("{what} is {} bytes, not {HASH_BYTES}", bytes.len()))
}

/// Decodes a `T` that takes up all of `bytes`.
///
/// Zebra's deserializers reject non-canonical lengths and encodings, so a
/// value that decodes and consumes every byte has the encoding the database
/// writes for it.
fn decode_all<T: ZcashDeserialize>(bytes: &[u8], what: &str) -> Result<T, String> {
    let mut reader = bytes;
    let decoded =
        T::zcash_deserialize(&mut reader).map_err(|error| format!("invalid {what}: {error}"))?;
    if reader.is_empty() {
        Ok(decoded)
    } else {
        Err(format!("{} trailing bytes after the {what}", reader.len()))
    }
}

/// Decodes a bincode `T` that takes up all of `bytes`, and checks that it is in
/// its canonical encoding.
fn decode_canonical<T>(bytes: &[u8], what: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + IntoDisk<Bytes = Vec<u8>>,
{
    // The database's `IntoDisk` writes these with the default options, which reject trailing
    // bytes.
    let decoded: T = bincode::DefaultOptions::new()
        .deserialize(bytes)
        .map_err(|error| format!("invalid {what}: {error}"))?;
    if decoded.as_bytes() != bytes {
        return Err(format!("{what} is not in its canonical encoding"));
    }
    Ok(decoded)
}

/// Checks that a tree's cached root is its recomputed root.
fn cached_root<R: PartialEq>(cached: Option<R>, recomputed: R, what: &str) -> Result<(), String> {
    match cached {
        Some(cached) if cached == recomputed => Ok(()),
        Some(_) => Err(format!("{what} has a cached root that is not its root")),
        None => Err(format!("{what} has no cached root")),
    }
}

/// A stored note commitment tree, with its nodes as bytes.
///
/// zebra-chain serializes its note commitment trees through legacy frontier
/// types (each tree's `legacy` module) and converts them back infallibly,
/// panicking on a frontier with the wrong number of ommers for its position or
/// a position past the tree's depth, and on a Sapling node, Sapling root or
/// Orchard root that is not a canonical field element. The importer decodes
/// this mirror of that layout first and refuses such a value. The fields are
/// in the legacy types' order: bincode identifies fields by position.
#[derive(Deserialize)]
struct RawTree {
    frontier: Option<RawFrontier>,
    cached_root: Option<[u8; 32]>,
}

/// The non-empty frontier of a [`RawTree`].
#[derive(Deserialize)]
struct RawFrontier {
    position: u64,
    leaf: RawLeaf,
    ommers: Vec<[u8; 32]>,
}

/// The rightmost leaf of a [`RawFrontier`]: a left child, or a right child
/// with its left sibling.
#[derive(Deserialize)]
enum RawLeaf {
    Left([u8; 32]),
    Right([u8; 32], [u8; 32]),
}

/// Checks that the note commitment tree `bytes` encodes converts to a tree
/// without panicking: its position is below `2^depth`, its frontier has one
/// ommer for each set bit of its position (a right leaf holds the first), and
/// `is_node` and `is_root` accept every node and the cached root.
fn check_tree_layout(
    bytes: &[u8],
    depth: u8,
    is_node: impl Fn(&[u8; 32]) -> bool,
    is_root: impl Fn(&[u8; 32]) -> bool,
    what: &str,
) -> Result<(), String> {
    let raw: RawTree = bincode::DefaultOptions::new()
        .deserialize(bytes)
        .map_err(|error| format!("invalid {what}: {error}"))?;
    if let Some(frontier) = &raw.frontier {
        if frontier.position >> depth != 0 {
            return Err(format!(
                "{what} position {} is past the tree's depth",
                frontier.position
            ));
        }
        let (leaves, right) = match &frontier.leaf {
            RawLeaf::Left(leaf) => (vec![leaf], 0),
            RawLeaf::Right(left, leaf) => (vec![left, leaf], 1),
        };
        let ommers = frontier.ommers.len().saturating_add(right);
        // A `u64` has at most 64 set bits.
        let expected = frontier.position.count_ones() as usize;
        if ommers != expected {
            return Err(format!(
                "{what} has {ommers} ommers, not the {expected} of position {}",
                frontier.position
            ));
        }
        if !leaves.into_iter().chain(&frontier.ommers).all(is_node) {
            return Err(format!("{what} has a node that is not a field element"));
        }
    }
    if raw.cached_root.is_some_and(|root| !is_root(&root)) {
        return Err(format!(
            "{what} has a cached root that is not a field element"
        ));
    }
    Ok(())
}

/// Decodes a Sprout note commitment tree, recomputing its root.
pub(super) fn sprout_tree(bytes: &[u8]) -> Result<sprout::tree::NoteCommitmentTree, String> {
    // Sprout nodes and roots are plain hashes.
    check_tree_layout(
        bytes,
        SPROUT_MERKLE_DEPTH,
        |_| true,
        |_| true,
        "Sprout tree",
    )?;
    let tree: sprout::tree::NoteCommitmentTree = decode_canonical(bytes, "Sprout tree")?;
    cached_root(tree.cached_root(), tree.recalculate_root(), "Sprout tree")?;
    Ok(tree)
}

/// Decodes a Sapling note commitment tree, recomputing its root.
pub(super) fn sapling_tree(bytes: &[u8]) -> Result<sapling::tree::NoteCommitmentTree, String> {
    check_tree_layout(
        bytes,
        SAPLING_MERKLE_DEPTH,
        |node| {
            Option::<sapling_crypto::Node>::from(sapling_crypto::Node::from_bytes(*node)).is_some()
        },
        |root| sapling::tree::Root::try_from(*root).is_ok(),
        "Sapling tree",
    )?;
    let tree: sapling::tree::NoteCommitmentTree = decode_canonical(bytes, "Sapling tree")?;
    cached_root(tree.cached_root(), tree.recalculate_root(), "Sapling tree")?;
    Ok(tree)
}

/// Decodes an Orchard or Ironwood note commitment tree, recomputing its root.
pub(super) fn orchard_tree(bytes: &[u8]) -> Result<orchard::tree::NoteCommitmentTree, String> {
    check_tree_layout(
        bytes,
        ORCHARD_MERKLE_DEPTH,
        |node| orchard::tree::Node::try_from(&node[..]).is_ok(),
        |root| orchard::tree::Root::try_from(*root).is_ok(),
        "note commitment tree",
    )?;
    let tree: orchard::tree::NoteCommitmentTree = decode_canonical(bytes, "note commitment tree")?;
    cached_root(
        tree.cached_root(),
        tree.recalculate_root(),
        "note commitment tree",
    )?;
    Ok(tree)
}

/// Decodes the parts of a history tree, before they are checked against the
/// network.
pub(super) fn history_tree_parts(bytes: &[u8]) -> Result<HistoryTreeParts, String> {
    decode_canonical(bytes, "history tree")
}

#[cfg(test)]
pub(super) use crate::service::finalized_state::RawHistoryTree;

/// Checks that history tree peaks can be combined without overflowing or panicking.
pub(super) fn check_history_tree_peaks(
    bytes: &[u8],
    upgrade: NetworkUpgrade,
) -> Result<(), String> {
    crate::service::finalized_state::check_history_tree_peaks(bytes, upgrade)
}

/// Decodes chain value pools in the current encoding.
pub(super) fn value_pools(bytes: &[u8]) -> Result<ValueBalance<NonNegative>, String> {
    width(bytes, VALUE_BALANCE_BYTES, "value pools")?;
    ValueBalance::from_bytes(bytes).map_err(|error| format!("invalid value pools: {error}"))
}

/// Decodes a block info record in the current encoding.
pub(super) fn block_info(bytes: &[u8]) -> Result<BlockInfo, String> {
    width(bytes, BLOCK_INFO_BYTES, "block info")?;
    let (pools, rest) = bytes.split_at(VALUE_BALANCE_BYTES - 8);
    let (size, nsm) = rest.split_at(4);
    let mut value_pools = ValueBalance::<NonNegative>::from_bytes(pools)
        .map_err(|error| format!("invalid block info value pools: {error}"))?;
    let nsm = Amount::<NonNegative>::from_bytes(
        nsm.try_into()
            .map_err(|_| "block info NSM pool is not 8 bytes".to_string())?,
    )
    .map_err(|error| format!("invalid block info NSM pool: {error}"))?;
    value_pools.set_nsm_amount(nsm);
    let size = u32::from_le_bytes(
        size.try_into()
            .map_err(|_| "block info size is not 4 bytes".to_string())?,
    );
    Ok(BlockInfo::new(value_pools, size))
}

/// Decodes a height key or value, which `check_record` already checked the width of.
pub(super) fn height(bytes: &[u8]) -> Height {
    crate::FromDisk::from_bytes(bytes)
}

/// Decodes a transaction location, which `check_record` already checked the width of.
pub(super) fn transaction_location(bytes: &[u8]) -> TransactionLocation {
    crate::FromDisk::from_bytes(bytes)
}

/// Decodes an output location, which `check_record` already checked the width of.
pub(super) fn output_location(bytes: &[u8]) -> OutputLocation {
    crate::FromDisk::from_bytes(bytes)
}
