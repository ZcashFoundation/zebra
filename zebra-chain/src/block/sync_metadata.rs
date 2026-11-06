//! Per-block synchronization metadata: the entries of a version 2
//! `get-hashes` response (zips#1344, "`get-hashes`").
//!
//! The values are *scheduling hints*: a synchronizing node uses them to size
//! download units before fetching blocks, and IBD-3 uses `txouts` to locate
//! each block's bits in the spentness-hint bitmap. They are only trusted once
//! the chunk that carries them matches its pin in the known-hash list.
//!
//! They are defined here, and encoded here, because the state that produces
//! them, the network that serves them, and the known-hash pins that
//! authenticate them must all agree on the same canonical bytes.

use std::io;

use crate::{
    block,
    serialization::{CompactSize64, SerializationError, ZcashDeserialize, ZcashSerialize},
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// One `get-hashes` response entry: the best-chain block at one height, with
/// its sync-cost metadata.
///
/// Encoded as `hash[32] ‖ size CS ‖ txs CS ‖ txouts CS ‖ notes CS`, where
/// `CS` is a canonical CompactSize.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct SyncHashEntry {
    /// The hash of the block at this entry's height.
    pub hash: block::Hash,

    /// The block's serialized size, in bytes.
    pub size: u64,

    /// The number of transactions in the block, including the coinbase.
    pub txs: u64,

    /// The number of transparent outputs created by the block's
    /// transactions, including coinbase outputs.
    pub txouts: u64,

    /// The number of note commitments the block adds: two per JoinSplit,
    /// one per Sapling output, one per Orchard or Ironwood action.
    pub notes: u64,
}

impl ZcashSerialize for SyncHashEntry {
    /// Writes `hash ‖ size ‖ txs ‖ txouts ‖ notes`, the counts as canonical
    /// CompactSizes.
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.hash.zcash_serialize(&mut writer)?;
        for count in [self.size, self.txs, self.txouts, self.notes] {
            CompactSize64::from(count).zcash_serialize(&mut writer)?;
        }
        Ok(())
    }
}

impl ZcashDeserialize for SyncHashEntry {
    /// Reads one entry; a non-canonical CompactSize is an error.
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let hash = block::Hash::zcash_deserialize(&mut reader)?;

        // `CompactSize64` rejects non-canonical encodings.
        let mut count = || CompactSize64::zcash_deserialize(&mut reader).map(u64::from);

        Ok(SyncHashEntry {
            hash,
            size: count()?,
            txs: count()?,
            txouts: count()?,
            notes: count()?,
        })
    }
}

/// Writes the canonical `get-hashes` response body for `entries`:
/// `CompactSize n ‖ n × entry`.
///
/// This is both the wire response body and, for 50,000-height chunks, the
/// exact bytes a known-hash pin hashes (design §5, spec issue S4).
pub fn write_sync_hashes_body<W: io::Write>(
    entries: &[SyncHashEntry],
    mut writer: W,
) -> Result<(), io::Error> {
    // `usize` fits in `u64` on every supported target.
    CompactSize64::from(entries.len() as u64).zcash_serialize(&mut writer)?;
    for entry in entries {
        entry.zcash_serialize(&mut writer)?;
    }
    Ok(())
}

/// Reads a complete `get-hashes` response body.
///
/// # Errors
///
/// - the count is above `max_count`;
/// - any CompactSize is non-canonical;
/// - the body is truncated, or has trailing bytes.
pub fn read_sync_hashes_body(
    mut body: &[u8],
    max_count: u32,
) -> Result<Vec<SyncHashEntry>, SerializationError> {
    let count = u64::from(CompactSize64::zcash_deserialize(&mut body)?);
    if count > u64::from(max_count) {
        return Err(SerializationError::Parse(
            "get-hashes entry count exceeds the maximum",
        ));
    }

    let entries = read_sync_hash_entries(&mut body, count)?;

    if !body.is_empty() {
        return Err(SerializationError::Parse(
            "trailing bytes after a get-hashes body",
        ));
    }

    Ok(entries)
}

/// Reads `count` [`SyncHashEntry`]s from the front of `body`, advancing it past
/// them.
///
/// This is the shared entry loop of every `get-hashes` body decoder, so the wire
/// decoder and [`read_sync_hashes_body`] parse the same bytes the same way.
/// Callers check `count` against their bound first, and check for trailing
/// bytes afterwards.
///
/// # Errors
///
/// A truncated body or a non-canonical CompactSize.
pub fn read_sync_hash_entries(
    body: &mut &[u8],
    count: u64,
) -> Result<Vec<SyncHashEntry>, SerializationError> {
    // Grown as entries arrive, not reserved from the count, so a short body
    // claiming the maximum count allocates nothing.
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push(SyncHashEntry::zcash_deserialize(&mut *body)?);
    }
    Ok(entries)
}

impl SyncHashEntry {
    /// Returns the entry for `block`, whose serialized size is
    /// `serialized_size`.
    ///
    /// This is the only definition of the counts: the state's sync metadata
    /// and the `known-hashes` pinning tool must both derive them here, so
    /// that a node serving `get-hashes` and a pinned chunk agree byte for
    /// byte.
    ///
    /// - `size` is the serialized length;
    /// - `txs` counts every transaction, including the coinbase;
    /// - `txouts` counts every transparent output, including coinbase
    ///   outputs;
    /// - `notes` counts the block's Sprout, Sapling, Orchard and Ironwood
    ///   note commitments.
    pub fn for_block(block: &block::Block, serialized_size: usize) -> Self {
        let txouts: usize = block.transactions.iter().map(|tx| tx.outputs().len()).sum();
        let notes = block.sprout_note_commitments().count()
            + block.sapling_note_commitments().count()
            + block.orchard_note_commitments().count()
            + block.ironwood_note_commitments().count();

        // The casts below are safe: Zebra only supports platforms where
        // `usize` is at most 64 bits, so every `usize` fits in a `u64`.
        SyncHashEntry {
            hash: block.hash(),
            size: serialized_size as u64,
            txs: block.transactions.len() as u64,
            txouts: txouts as u64,
            notes: notes as u64,
        }
    }
}
