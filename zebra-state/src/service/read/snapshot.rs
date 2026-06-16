//! Reading code for the P2P snapshot-distribution artifacts.
//!
//! These functions generate, on demand and from the finalized state, the
//! deterministic byte artifacts served over P2P by the inbound service:
//!
//! - **known-hash chunks** ([`known_hash_chunk_bytes`]): a `chunk_v2` encoding
//!   of a 150,000-block span (block hashes, size hints, and the sapling/orchard
//!   tree roots that update within the span);
//! - **note commitment trees** ([`note_commitment_tree_bytes`]): the canonical
//!   on-disk serialization of a sapling/orchard tree frontier as of a height;
//! - **snapshot ranges** ([`unspent_outputs_range`], [`address_balances_range`]):
//!   a byte range of the sorted unspent-output-location set or the sorted
//!   address-balance set at the finalized tip.
//!
//! Every artifact is a deterministic function of the chain, so every honest node
//! produces byte-identical output that hashes to the pinned SHA-256 constants in
//! `zebra-chain`. See `docs/design/p2p-snapshot-distribution.md`.

use zebra_chain::parameters::known_hashes::{
    chunk_v2::{self, TreeRoot},
    HASHES_PER_CHUNK,
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
/// [`transparent::Address`](zebra_chain::transparent::Address) followed by the
/// 32-byte `AddressBalanceLocation` value (balance + first-output-location +
/// received). The set is the sorted concatenation of these records.
pub const ADDRESS_BALANCE_RECORD_LEN: u64 = 21 + 32;

/// The maximum number of bytes a single snapshot range request may return.
///
/// This keeps each [`Response::SnapshotRange`](crate::response::Response) well
/// under the 2 MiB protocol frame, and bounds the work and memory a single
/// untrusted request can cost the server.
pub const MAX_SNAPSHOT_RANGE_BYTES: u64 = 1024 * 1024;

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
/// entirely in the future), so the caller answers `NotFound` instead of an empty
/// chunk that could never match a pinned hash.
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

/// Returns the requested byte range of the sorted unspent-output-location set at
/// the finalized tip, or `None` if the request is malformed or out of bounds.
///
/// The set is the ascending concatenation of fixed-size
/// [`UNSPENT_OUTPUT_RECORD_LEN`]-byte records. Returns `None` when `len` exceeds
/// [`MAX_SNAPSHOT_RANGE_BYTES`], or when `offset`/`len` are not record-aligned,
/// or when the requested range starts past the end of the set, so the caller
/// answers `NotFound` rather than dropping the peer.
pub fn unspent_outputs_range(db: &ZebraDb, offset: u64, len: u32) -> Option<Vec<u8>> {
    let len = u64::from(len);
    if len > MAX_SNAPSHOT_RANGE_BYTES
        || offset % UNSPENT_OUTPUT_RECORD_LEN != 0
        || len % UNSPENT_OUTPUT_RECORD_LEN != 0
    {
        return None;
    }

    let start_record = offset / UNSPENT_OUTPUT_RECORD_LEN;
    let wanted_records = len / UNSPENT_OUTPUT_RECORD_LEN;

    // Stream the set, copying only the requested window. Record indices are
    // counted so we never materialize the multi-million-entry set in memory.
    let mut out: Vec<u8> = Vec::with_capacity(len as usize);
    let mut index: u64 = 0;
    db.for_each_unspent_output_location_bytes(|record| {
        if index >= start_record && (index - start_record) < wanted_records {
            out.extend_from_slice(record);
        }
        index += 1;
    });

    // The requested range begins past the end of the set: there is nothing to
    // serve. A range that begins inside the set but runs past the end returns a
    // short final range (the assembled set is verified by its total hash).
    if start_record > index {
        return None;
    }

    Some(out)
}

/// Returns the requested byte range of the sorted address-balance set at the
/// finalized tip, or `None` if the request is malformed or out of bounds.
///
/// The set is the ascending-by-address concatenation of fixed-size
/// [`ADDRESS_BALANCE_RECORD_LEN`]-byte records (21-byte address + 32-byte
/// balance-location value). Bounds are enforced exactly as in
/// [`unspent_outputs_range`].
pub fn address_balances_range(db: &ZebraDb, offset: u64, len: u32) -> Option<Vec<u8>> {
    let len = u64::from(len);
    if len > MAX_SNAPSHOT_RANGE_BYTES
        || offset % ADDRESS_BALANCE_RECORD_LEN != 0
        || len % ADDRESS_BALANCE_RECORD_LEN != 0
    {
        return None;
    }

    let start_record = offset / ADDRESS_BALANCE_RECORD_LEN;
    let wanted_records = len / ADDRESS_BALANCE_RECORD_LEN;

    let mut out: Vec<u8> = Vec::with_capacity(len as usize);
    let mut index: u64 = 0;
    db.for_each_address_balance_bytes(|address_bytes, value_bytes| {
        if index >= start_record && (index - start_record) < wanted_records {
            out.extend_from_slice(address_bytes);
            out.extend_from_slice(value_bytes);
        }
        index += 1;
    });

    if start_record > index {
        return None;
    }

    Some(out)
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
        );

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

    /// A chunk index entirely above the finalized tip returns `None`, so the
    /// inbound handler answers `NotFound` rather than an unmatchable empty chunk.
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

    /// Snapshot range requests enforce their size and alignment bounds, returning
    /// `None` (so the peer is never dropped) for malformed or over-limit ranges.
    #[test]
    fn snapshot_range_bounds_are_enforced() {
        let _init_guard = zebra_test::init();

        let state = populated_mainnet_state();
        let db = &state.db;

        // Over the per-request byte limit.
        let over_limit = (MAX_SNAPSHOT_RANGE_BYTES + UNSPENT_OUTPUT_RECORD_LEN) as u32;
        assert_eq!(
            unspent_outputs_range(db, 0, over_limit),
            None,
            "an over-limit unspent-output range is rejected",
        );
        assert_eq!(
            address_balances_range(db, 0, over_limit),
            None,
            "an over-limit address-balance range is rejected",
        );

        // Misaligned offset / length (not a record multiple).
        assert_eq!(
            unspent_outputs_range(db, 1, UNSPENT_OUTPUT_RECORD_LEN as u32),
            None,
            "a misaligned unspent-output offset is rejected",
        );
        assert_eq!(
            unspent_outputs_range(db, 0, 3),
            None,
            "a misaligned unspent-output length is rejected",
        );

        // A well-formed, in-bounds request from offset 0 succeeds and is record
        // aligned (it may be empty if the genesis blocks left no unspent outputs).
        let range = unspent_outputs_range(db, 0, MAX_SNAPSHOT_RANGE_BYTES as u32)
            .expect("an in-bounds unspent-output range is served");
        assert_eq!(
            range.len() as u64 % UNSPENT_OUTPUT_RECORD_LEN,
            0,
            "served unspent-output bytes are record aligned",
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
}
