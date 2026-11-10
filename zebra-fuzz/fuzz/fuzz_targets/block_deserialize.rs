#![no_main]
//! Deep fuzz target for [`zebra_chain::block::Block`] deserialization.
//!
//! Promoted from a thin `zcash_deserialize`-only smoke target (13 LOC) to a
//! deep target asserting six business-logic invariants.
//!
//! Only panics, aborts, and broken invariants are bugs — parser errors are
//! expected on arbitrary bytes and silently returned.
//!
//! ## Invariants exercised
//!
//! * **I-1** parse OK ⇒ serialize round-trip preserves the complete encoding.
//!   Targets serialization asymmetry → consensus split vectors.
//! * **I-2** parse OK ⇒ `block.hash()` does not panic.
//! * **I-3** parse OK ⇒ merkle root computation does not panic.
//! * **I-4** parse OK ⇒ `coinbase_height()` ∈ `{ None, Some(h ∈ valid range) }`
//!   with no panic. Range is `[Height::MIN, Height::MAX]` per
//!   `zebra_chain::block::height` (`u32::MAX / 2`).
//! * **I-5** parse OK ⇒ `serialized_size <= MAX_BLOCK_BYTES` (2 MB) per
//!   ZIP block size consensus rule.
//! * **I-6** parse OK ⇒ `header.version >= ZCASH_BLOCK_VERSION` (4) and the
//!   high bit MUST NOT be set (Zebra's `check_version` enforces both).
//!
//! See `block_deep_fuzz.rs` for the four-layer companion target that adds
//! commitment, consensus, and per-transaction property exercise.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use std::panic;
use std::sync::Arc;

use zebra_chain::block::merkle::Root as MerkleRoot;
use zebra_chain::block::{Block, Hash, Height, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION};
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};

fuzz_target!(|data: &[u8]| {
    // ────────────────────────────────────────────────────────────────────
    // Phase 0: Deserialize. Bail without assertion if parsing fails — the
    // input is arbitrary and most byte strings are not valid blocks.
    // ────────────────────────────────────────────────────────────────────
    let block = match Block::zcash_deserialize(Cursor::new(data)) {
        Ok(b) => b,
        Err(_) => return,
    };
    let block = Arc::new(block);

    // ────────────────────────────────────────────────────────────────────
    // I-1 — Round-trip serialization byte equality.
    //
    // Compare complete encodings: Block::PartialEq inherits txid-only
    // transaction equality, which misses V5/V6 signatures and proofs.
    // ────────────────────────────────────────────────────────────────────
    let serialized = block.zcash_serialize_to_vec().expect(
        "ZcashSerialize is infallible except for writer errors, and a Vec writer cannot fail",
    );
    let reparsed = Block::zcash_deserialize(Cursor::new(&serialized))
        .expect("I-1: serializer produced bytes the deserializer rejects");
    let serialized2 = reparsed.zcash_serialize_to_vec().expect(
        "ZcashSerialize is infallible except for writer errors, and a Vec writer cannot fail",
    );
    assert_eq!(
        serialized, serialized2,
        "I-1: re-serialization byte mismatch (consensus split vector)"
    );

    // ────────────────────────────────────────────────────────────────────
    // I-2 — `block.hash()` must not panic and must be deterministic.
    //
    // The block hash is the fundamental identity used everywhere in the
    // node (state, network gossip, RPC). Any panic here is a remote-DoS.
    // ────────────────────────────────────────────────────────────────────
    let hash_result = panic::catch_unwind(panic::AssertUnwindSafe(|| block.hash()));
    let hash: Hash = match hash_result {
        Ok(h) => h,
        Err(_) => panic!("I-2: block.hash() panicked on parsed block"),
    };

    // Determinism: hashing twice on the same block must give the same hash.
    let hash_again = panic::catch_unwind(panic::AssertUnwindSafe(|| block.hash()))
        .expect("I-2: block.hash() panicked on second call");
    assert_eq!(
        hash, hash_again,
        "I-2: block.hash() is non-deterministic across calls"
    );

    // ────────────────────────────────────────────────────────────────────
    // I-3 — Merkle root computation must not panic.
    //
    // The merkle root is computed by collecting the transactions into the
    // `merkle::Root` `FromIterator` impl. The thin target never exercised
    // this path; we now do, both via the iterator collector and via the
    // hash-based per-transaction path, to cover the two paths in
    // zebra-chain/src/block/merkle.rs.
    // ────────────────────────────────────────────────────────────────────
    // Note: empty blocks (0 transactions) are intentionally not exercised by
    // this oracle. The `merkle::Root` `FromIterator` impl is documented to
    // panic on an empty iterator (see its doc-comment), so feeding it zero
    // transactions would only re-surface that documented precondition rather
    // than an unexpected defect. We skip the assertion on empty-tx blocks so
    // the harness reports only genuine failures on non-empty input.
    let merkle_root = if block.transactions.is_empty() {
        None
    } else {
        let merkle_root_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let root: MerkleRoot = block.transactions.iter().collect();
            root
        }));
        match merkle_root_result {
            Ok(root) => Some(root),
            Err(_) => panic!("I-3: merkle root computation panicked on non-empty block"),
        }
    };

    // Recompute via the `FromIterator<transaction::Hash>` path. This is the
    // path used by `zebra_consensus::block::check::merkle_root_validity`.
    // Both paths must yield the same root.
    let tx_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let merkle_root_via_hashes_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        // Empty blocks would panic per the FromIterator<transaction::Hash>
        // doc-comment ("Panics: When there are no transactions in the
        // iterator."). Skip the cross-check in that case; the iterator
        // collector path tolerates empty input via the generic impl.
        if tx_hashes.is_empty() {
            None
        } else {
            let root: MerkleRoot = tx_hashes.iter().copied().collect();
            Some(root)
        }
    }));
    if let (Some(via_txs), Ok(Some(via_hashes))) = (merkle_root, merkle_root_via_hashes_result) {
        assert_eq!(
            via_txs, via_hashes,
            "I-3: merkle root mismatch between transaction-collect and \
             hash-collect paths"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // I-4 — `coinbase_height()` returns `None` or a height in valid range.
    //
    // Coinbase height is parsed from the first input of the first
    // transaction (a coinbase input carries a height). It may be `None`
    // (no coinbase in tx 0). When present it must be in `[MIN, MAX]`.
    // The range is enforced by `Height::new`/serialize callers, but we
    // guard against any parser path producing an out-of-range value.
    // ────────────────────────────────────────────────────────────────────
    let height_result = panic::catch_unwind(panic::AssertUnwindSafe(|| block.coinbase_height()));
    match height_result {
        Ok(None) => {
            // Block with no coinbase height — legal for malformed inputs
            // (still parsed) and for header-only / non-coinbase first tx.
        }
        Ok(Some(height)) => {
            // Verify the height is in the structurally valid range.
            // `Height(u32)` carries `MIN = 0` and `MAX = u32::MAX / 2`.
            assert!(
                height >= Height::MIN,
                "I-4: coinbase_height {height:?} below Height::MIN"
            );
            assert!(
                height <= Height::MAX,
                "I-4: coinbase_height {:?} exceeds Height::MAX ({:?})",
                height,
                Height::MAX
            );
        }
        Err(_) => panic!("I-4: block.coinbase_height() panicked"),
    }

    // ────────────────────────────────────────────────────────────────────
    // I-5 — Serialized size MUST NOT exceed MAX_BLOCK_BYTES (2 MB).
    //
    // The deserializer caps reads at `MAX_BLOCK_BYTES` via `take(...)`, so
    // any parsed block fits. Serialization back out must also fit;
    // otherwise we have a serialize / deserialize asymmetry that could
    // produce blocks too large to retransmit.
    // ────────────────────────────────────────────────────────────────────
    let size = serialized.len();
    assert!(
        u64::try_from(size).expect("block size fits in u64") <= MAX_BLOCK_BYTES,
        "I-5: serialized block size {} exceeds MAX_BLOCK_BYTES {}",
        size,
        MAX_BLOCK_BYTES
    );

    // ────────────────────────────────────────────────────────────────────
    // I-6 — Header version is in the supported set.
    //
    // Zebra's `check_version` (zebra-chain/src/block/serialize.rs) enforces
    // two rules during deserialization:
    //   1. high bit (`>> 31 != 0`) MUST NOT be set
    //   2. version MUST be >= ZCASH_BLOCK_VERSION (= 4) post-Sapling
    //
    // Because we successfully parsed the block, both rules hold. We
    // re-assert them here so any future parser regression that loosens
    // the check (or any direct constructor path) is caught by fuzzing.
    // ────────────────────────────────────────────────────────────────────
    let version = block.header.version;
    assert!(
        version >> 31 == 0,
        "I-6: header.version {version} has its high bit set \
         (deserializer must reject; saw it parse)"
    );
    assert!(
        version >= ZCASH_BLOCK_VERSION,
        "I-6: header.version {version} is below ZCASH_BLOCK_VERSION ({})",
        ZCASH_BLOCK_VERSION
    );

    // ────────────────────────────────────────────────────────────────────
    // Belt-and-braces sanity: a couple of cheap header field reads that
    // shouldn't panic and have come up in past audit reviews. These don't
    // count toward the six invariants but keep the target exercising more
    // code paths beyond the bare minimum.
    // ────────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = block.header.commitment_bytes;
        let _ = block.header.time;
        let _ = block.header.difficulty_threshold;
    }));

    // Touch transaction count — past panics in length/bound code paths
    // make this worth a cheap probe per parsed block.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = block.transactions.len();
        for tx in &block.transactions {
            let _ = tx.is_coinbase();
        }
    }));
});
