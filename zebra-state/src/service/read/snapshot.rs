//! Reading code for the snapshot-distribution artifacts.
//!
//! These functions generate, on demand and from the finalized state, the
//! deterministic byte artifacts that `emit-snapshot` writes as release assets
//! and that the snapshot-consume IBD engine verifies and loads:
//!
//! - **known-hash chunks** ([`known_hash_chunk_bytes`]): a `chunk_v2` encoding
//!   of a 150,000-block span (block hashes, size hints, and the sapling/orchard
//!   tree roots that update within the span);
//! - **note commitment trees** ([`note_commitment_tree_bytes`]): the canonical
//!   on-disk serialization of a sapling/orchard tree frontier as of a height.
//!
//! Every artifact is a deterministic function of the chain, so every honest node
//! produces byte-identical output that hashes to the pinned SHA-256 constants in
//! `zebra-chain`. See `docs/design/snapshot-distribution.md`.

use bincode::Options;

use zebra_chain::{
    orchard,
    parameters::known_hashes::{
        chunk_v2::{self, TreeRoot},
        HASHES_PER_CHUNK,
    },
    sapling,
};

use crate::{
    request::ShieldedPool,
    service::finalized_state::{IntoDisk, ZebraDb},
};

/// The fixed serialized length of one unspent-output-set record: an 8-byte
/// `OutputLocation`. The set is the sorted concatenation of these records, so a
/// byte offset is record-aligned iff it is a multiple of this length.
pub const UNSPENT_OUTPUT_RECORD_LEN: u64 = 8;

/// The fixed serialized length of one address-balance-set record: a 21-byte
/// [`transparent::Address`](zebra_chain::transparent::Address) key followed by
/// the 24-byte `AddressBalanceLocation` on-disk value (8-byte balance + 8-byte
/// first-output-location + 8-byte received total). The set is the sorted
/// concatenation of these fixed-size records.
///
/// The 24-byte value length is `AddressBalanceLocation`'s `IntoDisk` encoding
/// (`BALANCE_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES + size_of::<u64>()`), and
/// the 21-byte key is `transparent::Address`'s `IntoDisk` encoding (1 variant
/// byte + 20 hash bytes), so this matches exactly what
/// [`ZebraDb::for_each_address_balance_bytes`](crate::service::finalized_state::ZebraDb::for_each_address_balance_bytes)
/// emits per record.
pub const ADDRESS_BALANCE_RECORD_LEN: u64 = ADDRESS_BALANCE_KEY_LEN + ADDRESS_BALANCE_VALUE_LEN;

/// The serialized length of one address-balance record's key: the 21-byte
/// `transparent::Address` `IntoDisk` encoding (1 variant byte + 20 hash bytes).
pub const ADDRESS_BALANCE_KEY_LEN: u64 = 21;

/// The serialized length of one address-balance record's value: the 24-byte
/// `AddressBalanceLocation` `IntoDisk` encoding (8-byte balance + 8-byte
/// first-output-location + 8-byte received total).
pub const ADDRESS_BALANCE_VALUE_LEN: u64 = 24;

/// Returns the `chunk_v2` bytes for known-hash chunk `index`, or `None` if the
/// span has no blocks.
///
/// If the chunk has already been downloaded-and-verified into the
/// `known_hash_chunk` column family, its stored bytes are returned directly.
/// Otherwise the bytes are generated on demand from the finalized state.
///
/// A chunk covers the `HASHES_PER_CHUNK`-block span starting at
/// `index * HASHES_PER_CHUNK`. The span is truncated at the finalized tip, so a
/// not-yet-synced node serves only the prefix it has; a node that is past the
/// span end serves the full span. The returned bytes are deterministic: a
/// content-addressed requester verifies their SHA-256 against the pinned
/// `chunk_hashes[index]` constant.
///
/// Returns `None` when the span base is above the finalized tip (the chunk is
/// entirely in the future), so the caller reports the chunk as unavailable
/// instead of producing an empty chunk that could never match a pinned hash.
pub fn known_hash_chunk_bytes(db: &ZebraDb, index: u32) -> Option<Vec<u8>> {
    // Prefer the stored, already-verified chunk if it is resident in the CF.
    if let Some(stored) = db.known_hash_chunk(index) {
        return Some(stored);
    }

    let tip_height = db.finalized_tip_height()?;

    // The span base is `index * HASHES_PER_CHUNK`. Use a checked multiply on the
    // untrusted `index` so a huge index can't overflow into a valid-looking
    // height; an overflow means the span is far in the future, so `None`.
    let span_base = index.checked_mul(HASHES_PER_CHUNK)?;

    // Nothing in this span exists yet: the whole chunk is in the future.
    if span_base > tip_height.0 {
        return None;
    }

    // The span covers `[span_base, span_end]`, truncated at the finalized tip.
    // `span_base + HASHES_PER_CHUNK - 1` cannot overflow because `span_base` is
    // a multiple of `HASHES_PER_CHUNK` no greater than `tip_height.0` (a u32
    // block height), so it is well within `u32::MAX`.
    let span_last = (span_base + HASHES_PER_CHUNK - 1).min(tip_height.0);

    let mut blocks: Vec<([u8; 32], u8)> = Vec::with_capacity((span_last - span_base + 1) as usize);
    for height in span_base..=span_last {
        let block_hash = db.hash(zebra_chain::block::Height(height))?;

        // The size hint quantizes the serialized block size into
        // `size.div_ceil(SIZE_HINT_UNIT)`, capped at the `DEFAULT_SIZE_HINT`
        // loosest bound; it is always an upper bound on the block's size.
        let hint = db
            .block_and_size(zebra_chain::block::Height(height).into())
            .map(|(_block, size)| size_hint(size))
            .unwrap_or(chunk_v2::DEFAULT_SIZE_HINT);

        blocks.push((block_hash.0, hint));
    }

    let sapling_roots = sapling_tree_roots(db, span_base, span_last);
    let orchard_roots = orchard_tree_roots(db, span_base, span_last);

    Some(chunk_v2::encode(
        &blocks,
        true,
        &sapling_roots,
        &orchard_roots,
        true,
    ))
}

/// Quantizes a serialized block `size` into a `chunk_v2` size hint byte.
///
/// The hint `w` means "size is at most `w × SIZE_HINT_UNIT` bytes", so it is
/// always an upper bound. A zero-byte block (which cannot occur) and any size at
/// or below one unit both map to `1`; oversized values saturate at the loosest
/// `DEFAULT_SIZE_HINT` bound.
fn size_hint(size: usize) -> u8 {
    use zebra_chain::parameters::known_hashes::SIZE_HINT_UNIT;

    // `size.div_ceil(unit)` is the smallest `w` with `w * unit >= size`.
    // `SIZE_HINT_UNIT` is a small positive constant, so the division is safe.
    let quantized = (size as u64).div_ceil(SIZE_HINT_UNIT as u64).max(1);

    // Saturate at the loosest bound: hints above 255 don't fit in a byte, and
    // `DEFAULT_SIZE_HINT` (255) is the loosest valid upper bound anyway.
    // The cast is safe because the value is clamped to `1..=255` first.
    quantized.min(chunk_v2::DEFAULT_SIZE_HINT as u64) as u8
}

/// Collects the sapling tree-root update records for the span
/// `[span_base, span_last]`, as relative heights with the tree root at each
/// height that records a new tree, in ascending order.
fn sapling_tree_roots(db: &ZebraDb, span_base: u32, span_last: u32) -> Vec<TreeRoot> {
    use zebra_chain::block::Height;

    let mut roots = Vec::new();
    for (height, tree) in db.sapling_tree_by_height_range(Height(span_base)..=Height(span_last)) {
        // `height >= span_base` because the range starts at `span_base`, so the
        // subtraction does not underflow.
        let rel_height = height.0 - span_base;
        let root: [u8; 32] = (&tree.root()).into();
        roots.push(TreeRoot { rel_height, root });
    }
    roots
}

/// Collects the orchard tree-root update records for the span
/// `[span_base, span_last]`, like [`sapling_tree_roots`].
fn orchard_tree_roots(db: &ZebraDb, span_base: u32, span_last: u32) -> Vec<TreeRoot> {
    use zebra_chain::block::Height;

    let mut roots = Vec::new();
    for (height, tree) in db.orchard_tree_by_height_range(Height(span_base)..=Height(span_last)) {
        // `height >= span_base`, as in `sapling_tree_roots`.
        let rel_height = height.0 - span_base;
        let root: [u8; 32] = (&tree.root()).into();
        roots.push(TreeRoot { rel_height, root });
    }
    roots
}

/// Returns the canonical serialization of the `pool` note commitment tree as of
/// `height`, or `None` if `height` is above the finalized tip.
///
/// The serialization is the same deterministic on-disk encoding used by the
/// finalized state, so the requester's recomputed `.root()` matches the root
/// recorded in the relevant known-hash chunk.
pub fn note_commitment_tree_bytes(
    db: &ZebraDb,
    pool: ShieldedPool,
    height: zebra_chain::block::Height,
) -> Option<Vec<u8>> {
    match pool {
        ShieldedPool::Sapling => db
            .sapling_tree_by_height(&height)
            .map(|tree| tree.as_bytes()),
        ShieldedPool::Orchard => db
            .orchard_tree_by_height(&height)
            .map(|tree| tree.as_bytes()),
    }
}

/// Deserializes source-supplied `pool` note-commitment-tree `bytes` and returns
/// the tree's `.root()` as a 32-byte array, or `None` if the bytes do not
/// deserialize.
///
/// This is the inverse of [`note_commitment_tree_bytes`] for the
/// snapshot-consume path: the bytes come from an untrusted artifact file, so unlike the
/// state's internal `FromDisk` (which `expect`s a self-produced encoding), this
/// returns `None` on a malformed encoding rather than panicking. The caller
/// checks the returned root against the root recorded in the relevant
/// known-hash chunk before trusting the tree.
///
/// The encoding is the same `bincode::DefaultOptions` serialization that
/// [`note_commitment_tree_bytes`] produces (`tree.as_bytes()`), so an honest
/// artifact's bytes round-trip to a tree with the recorded root.
pub fn note_commitment_tree_root_from_bytes(pool: ShieldedPool, bytes: &[u8]) -> Option<[u8; 32]> {
    let options = bincode::DefaultOptions::new();
    match pool {
        ShieldedPool::Sapling => {
            let tree: sapling::tree::NoteCommitmentTree = options.deserialize(bytes).ok()?;
            Some((&tree.root()).into())
        }
        ShieldedPool::Orchard => {
            let tree: orchard::tree::NoteCommitmentTree = options.deserialize(bytes).ok()?;
            Some((&tree.root()).into())
        }
    }
}

/// Deserializes the optional source-supplied sapling and orchard note-commitment-tree
/// frontier `bytes` into a [`NoteCommitmentTrees`] for the snapshot-consume
/// "tree supplied by download" commit path
/// (`docs/design/snapshot-distribution.md` §3.2).
///
/// Returns `None` when neither pool's bytes are supplied, or when any supplied
/// bytes fail to deserialize (a corrupt or tampered artifact): the caller then
/// falls back to folding. A missing pool keeps its default (empty) tree — the
/// per-pool roots were already verified against the chunk's recorded roots by the
/// IBD engine's tree-fetch lookahead before these bytes were buffered, and the
/// commit re-verifies the supplied sapling root against the block header.
///
/// The sprout and subtree fields are left at their defaults: sprout is never
/// supplied (the snapshot payload carries only sapling/orchard, and the commit
/// folds sprout), and subtree completions are derived by the commit path because
/// the frontier blob does not carry them.
pub fn supplied_note_commitment_trees_from_bytes(
    sapling_bytes: Option<&[u8]>,
    orchard_bytes: Option<&[u8]>,
) -> Option<zebra_chain::parallel::tree::NoteCommitmentTrees> {
    if sapling_bytes.is_none() && orchard_bytes.is_none() {
        return None;
    }

    let options = bincode::DefaultOptions::new();
    let mut trees = zebra_chain::parallel::tree::NoteCommitmentTrees::default();

    if let Some(bytes) = sapling_bytes {
        let tree: sapling::tree::NoteCommitmentTree = options.deserialize(bytes).ok()?;
        // Cache the root so the commit path's `.root()` reads are free and the
        // header re-verification matches the engine's earlier check.
        let _ = tree.root();
        trees.sapling = std::sync::Arc::new(tree);
    }

    if let Some(bytes) = orchard_bytes {
        let tree: orchard::tree::NoteCommitmentTree = options.deserialize(bytes).ok()?;
        let _ = tree.root();
        trees.orchard = std::sync::Arc::new(tree);
    }

    Some(trees)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use zebra_chain::{
        block::{Block, Height},
        parameters::known_hashes::chunk_v2::ParsedChunk,
        parameters::Network,
        serialization::ZcashDeserializeInto,
    };

    use crate::{service::finalized_state::FinalizedState, Config};

    use super::*;

    /// Builds an ephemeral finalized state and commits mainnet genesis, block 1,
    /// and block 2 into it. Returns the populated state.
    fn populated_mainnet_state() -> FinalizedState {
        let mut state = FinalizedState::new(
            &Config::ephemeral(),
            &Network::Mainnet,
            #[cfg(feature = "elasticsearch")]
            false,
        )
        .expect("test database opens");

        let blocks = Network::Mainnet.blockchain_map();
        for height in 0..=2 {
            let block: Arc<Block> = blocks
                .get(&height)
                .expect("block height has test data")
                .zcash_deserialize_into()
                .expect("test data deserializes");

            state
                .commit_finalized_direct(block.into(), None, "snapshot serve test")
                .expect("test block is valid");
        }

        state
    }

    /// The generated chunk for index 0 parses as a valid v2 chunk whose block
    /// hashes and size hints match the state, with hints and tree roots present.
    #[test]
    fn known_hash_chunk_serve_round_trips() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        let bytes =
            known_hash_chunk_bytes(db, 0).expect("chunk 0 is generated for a populated state");

        let parsed = ParsedChunk::parse(&bytes).expect("generated chunk parses as v2");
        assert_eq!(parsed.block_count(), 3, "chunk covers heights 0..=2");
        assert!(parsed.has_hints(), "generated chunk carries size hints");

        for rel in 0..3 {
            let expected = db
                .hash(Height(rel))
                .expect("populated state has this height")
                .0;
            assert_eq!(
                parsed.block_hash(rel),
                expected,
                "chunk block hash at rel height {rel} matches the state",
            );
            assert_ne!(
                parsed.size_hint(rel),
                0,
                "size hint at rel height {rel} is a non-zero upper bound",
            );
        }

        // Generation is deterministic: a second call produces identical bytes.
        let bytes_again = known_hash_chunk_bytes(db, 0).expect("chunk 0 regenerates");
        assert_eq!(bytes, bytes_again, "chunk generation is deterministic");
    }

    /// The address-balance set emits fixed 45-byte records (21-byte key +
    /// 24-byte value), matching `ADDRESS_BALANCE_RECORD_LEN`, so the emitted
    /// set file and the consume-side framing stay record-aligned.
    #[test]
    fn address_balance_record_length_matches_emitted_bytes() {
        let _init_guard = zebra_test::init();

        assert_eq!(
            ADDRESS_BALANCE_RECORD_LEN, 45,
            "address-balance record is 21-byte key + 24-byte value",
        );

        let state = populated_mainnet_state();
        let db = &state.db;

        db.for_each_address_balance_bytes(|key, value| {
            assert_eq!(
                key.len() as u64,
                ADDRESS_BALANCE_KEY_LEN,
                "address key is exactly ADDRESS_BALANCE_KEY_LEN bytes",
            );
            assert_eq!(
                value.len() as u64,
                ADDRESS_BALANCE_VALUE_LEN,
                "balance value is exactly ADDRESS_BALANCE_VALUE_LEN bytes",
            );
        });
    }

    /// A chunk that has been written into the `known_hash_chunk` column family is
    /// served verbatim from the CF, taking precedence over on-demand generation.
    #[test]
    fn known_hash_chunk_serve_prefers_stored_cf_bytes() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        // A sentinel blob that is not a valid generated chunk for index 0.
        let stored: Vec<u8> = vec![0xCD; 16];
        db.write_known_hash_chunk(0, &stored)
            .expect("ephemeral db accepts the write");

        assert_eq!(
            known_hash_chunk_bytes(db, 0).as_deref(),
            Some(stored.as_slice()),
            "stored CF bytes are served verbatim",
        );
    }

    /// A chunk index entirely above the finalized tip returns `None`, rather
    /// than an unmatchable empty chunk.
    #[test]
    fn known_hash_chunk_serve_above_tip_is_none() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        // Index 1 starts at height 150,000, far above the 3-block tip.
        assert_eq!(
            known_hash_chunk_bytes(db, 1),
            None,
            "a chunk index above the tip returns None",
        );

        // A huge index would overflow the span-base multiply: also None.
        assert_eq!(
            known_hash_chunk_bytes(db, u32::MAX),
            None,
            "an overflowing chunk index returns None",
        );
    }

    /// Note commitment trees serialize deterministically and round-trip back to a
    /// tree with the same root; an above-tip height returns `None`.
    #[test]
    fn note_commitment_tree_serve_round_trips() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        for pool in [ShieldedPool::Sapling, ShieldedPool::Orchard] {
            let bytes = note_commitment_tree_bytes(db, pool, Height(2))
                .expect("tree exists at the finalized tip");
            let bytes_again =
                note_commitment_tree_bytes(db, pool, Height(2)).expect("tree regenerates");
            assert_eq!(bytes, bytes_again, "tree serialization is deterministic");

            assert_eq!(
                note_commitment_tree_bytes(db, pool, Height(1_000_000)),
                None,
                "an above-tip height returns None",
            );
        }
    }

    /// The served tree bytes deserialize back to a root matching the one the
    /// state records in the known-hash chunk for the same height, so a consuming
    /// node's `note_commitment_tree_root_from_bytes` check passes for an honest
    /// artifact's bytes and fails for garbage.
    #[test]
    fn served_tree_root_matches_chunk_record() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        // The chunk records the tree root at each height that updates the tree.
        let chunk_bytes = known_hash_chunk_bytes(db, 0).expect("chunk 0 is generated");
        let parsed = ParsedChunk::parse(&chunk_bytes).expect("chunk parses");

        for pool in [ShieldedPool::Sapling, ShieldedPool::Orchard] {
            for rel in 0..3u32 {
                let tree_bytes = note_commitment_tree_bytes(db, pool, Height(rel))
                    .expect("tree exists at this height");

                let served_root = note_commitment_tree_root_from_bytes(pool, &tree_bytes)
                    .expect("served tree bytes deserialize");

                // The chunk may not record a root at every height (only heights
                // that change the tree). When it does, the served root must match.
                let recorded = match pool {
                    ShieldedPool::Sapling => parsed.sapling_root_at_or_before(rel),
                    ShieldedPool::Orchard => parsed.orchard_root_at_or_before(rel),
                };
                if let Some(recorded) = recorded {
                    // `*_root_at_or_before` returns the root at the largest
                    // recorded height <= rel; for a height that records its own
                    // root, that equals this height's tree root.
                    let exact = match pool {
                        ShieldedPool::Sapling => parsed
                            .sapling_roots()
                            .into_iter()
                            .find(|r| r.rel_height == rel),
                        ShieldedPool::Orchard => parsed
                            .orchard_roots()
                            .into_iter()
                            .find(|r| r.rel_height == rel),
                    };
                    if let Some(record) = exact {
                        assert_eq!(
                            served_root, record.root,
                            "served {pool:?} tree root at rel {rel} matches the chunk record",
                        );
                    }
                    // `recorded` is used to confirm a root exists at or before rel.
                    let _ = recorded;
                }
            }

            // Garbage never deserializes to a root.
            assert_eq!(
                note_commitment_tree_root_from_bytes(pool, &[0xFF; 7]),
                None,
                "garbage tree bytes return None",
            );
        }
    }
}
