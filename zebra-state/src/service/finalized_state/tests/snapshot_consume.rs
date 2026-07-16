//! Tests for the snapshot-consume (assumeUTXO) finalized write path.
//!
//! Covers:
//! - the direct note-commitment-tree write: a checkpoint block committed with
//!   pre-fetched trees supplied writes those trees directly into the tree column
//!   families instead of folding them, and the on-disk result matches a normal
//!   folded commit;
//! - `H_max` parity: a small chain committed through the snapshot-consume write
//!   path (survivor-only UTXO elision on, per-block balances/value-pools skipped
//!   then bulk-loaded at `H_max`) has a byte-identical UTXO set, chain value
//!   pools, and address balances to the same chain committed normally;
//! - a zero-`MissingTransparentOutput` / no-panic anti-regression for checkpoint
//!   commits with elision on (the spend-value reads that previously crashed are
//!   gone).

use std::sync::Arc;

use zebra_chain::{
    amount::NonNegative,
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    value_balance::ValueBalance,
};

use zebra_chain::parallel::tree::NoteCommitmentTrees;

use crate::{
    service::finalized_state::FinalizedState,
    snapshot_consume::{SnapshotConsumeState, SurvivorSet},
    CheckpointVerifiedBlock, Config,
};

/// Commits the mainnet genesis and block 1 to a fresh finalized state and
/// returns it together with block 1.
fn fresh_state_with_genesis(network: &Network) -> (FinalizedState, Arc<Block>) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis deserializes");
    let block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes");

    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("test database opens");

    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis).into(),
            None,
            "snapshot-consume test genesis",
        )
        .expect("genesis commits");

    (state, block1)
}

/// Supplied-tree write round-trip: committing block 1 with its note commitment
/// trees supplied produces the same on-disk trees as folding them normally.
///
/// Block 1 is in the pre-Sapling era, where the block header does not pin this
/// block's note commitment trees, so the supplied trees are *refused* and folded
/// instead (finding #27) — but the on-disk result is identical, which is what
/// makes refusing-and-folding safe. The accept-and-verify path (Sapling era) and
/// the reject-on-mismatch path are covered by
/// [`super::super::supplied_trees_are_verifiable`]'s unit tests below.
#[test]
fn supplied_tree_write_round_trips() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Baseline: commit block 1 normally (folding the trees).
    let (mut baseline_state, block1) = fresh_state_with_genesis(&network);
    let (_hash, folded_trees) = baseline_state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block1.clone()).into(),
            None,
            "baseline folded commit",
        )
        .expect("block 1 commits by folding");

    let baseline_sapling = baseline_state
        .db
        .sapling_tree_by_height(&Height(1))
        .expect("baseline sapling tree at height 1");
    let baseline_orchard = baseline_state
        .db
        .orchard_tree_by_height(&Height(1))
        .expect("baseline orchard tree at height 1");

    // Supplied: commit the same block 1, but supply the trees directly. The
    // result must match the folded trees byte-for-byte.
    let (mut supplied_state, block1) = fresh_state_with_genesis(&network);
    supplied_state
        .commit_finalized_direct_with_trees(
            CheckpointVerifiedBlock::from(block1).into(),
            None,
            Some(folded_trees.clone()),
            "supplied tree commit",
        )
        .expect("block 1 commits with supplied trees");

    let supplied_sapling = supplied_state
        .db
        .sapling_tree_by_height(&Height(1))
        .expect("supplied sapling tree at height 1");
    let supplied_orchard = supplied_state
        .db
        .orchard_tree_by_height(&Height(1))
        .expect("supplied orchard tree at height 1");

    assert_eq!(
        baseline_sapling.root(),
        supplied_sapling.root(),
        "supplied sapling tree root matches the folded tree",
    );
    assert_eq!(
        baseline_orchard.root(),
        supplied_orchard.root(),
        "supplied orchard tree root matches the folded tree",
    );
    assert_eq!(
        baseline_sapling, supplied_sapling,
        "supplied sapling tree is written byte-identically",
    );
    assert_eq!(
        baseline_orchard, supplied_orchard,
        "supplied orchard tree is written byte-identically",
    );

    // Both states reached the same tip.
    assert_eq!(
        baseline_state.db.finalized_tip_hash(),
        supplied_state.db.finalized_tip_hash(),
        "both commits reach the same tip",
    );
}

/// Collects the on-disk UTXO set (sorted 8-byte output locations), the chain
/// value pools, and the address-balance set (sorted key||value bytes) of a
/// finalized state, for byte-comparison.
fn collect_state(
    state: &FinalizedState,
) -> (Vec<Vec<u8>>, ValueBalance<NonNegative>, Vec<Vec<u8>>) {
    let mut utxo_set = Vec::new();
    state
        .db
        .for_each_unspent_output_location_bytes(|loc| utxo_set.push(loc.to_vec()));

    let value_pools = state.db.finalized_value_pool();

    let mut balances = Vec::new();
    state.db.for_each_address_balance_bytes(|key, value| {
        let mut record = key.to_vec();
        record.extend_from_slice(value);
        balances.push(record);
    });

    (utxo_set, value_pools, balances)
}

/// Commits the mainnet genesis and blocks `1..=max_height` to a fresh finalized
/// state normally (folding, deriving balances and value pools). Returns the
/// state and the committed blocks.
fn commit_blocks_normally(network: &Network, max_height: u32) -> (FinalizedState, Vec<Arc<Block>>) {
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("test database opens");

    let mut blocks = Vec::new();
    for height in 0..=max_height {
        let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .get(&height)
            .unwrap_or_else(|| panic!("continuous mainnet block {height} exists"))
            .zcash_deserialize_into()
            .expect("block deserializes");

        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "normal commit",
            )
            .unwrap_or_else(|error| panic!("block {height} commits normally: {error:?}"));

        blocks.push(block);
    }

    (state, blocks)
}

/// `H_max` parity: a chain committed through the snapshot-consume write path
/// (survivor-only UTXO elision on, per-block balances and value pools skipped
/// then bulk-loaded at `H_max`) has a UTXO set, chain value pools, and address
/// balances byte-identical to the same chain committed normally.
#[test]
fn snapshot_consume_h_max_parity() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 10;

    // Baseline: commit blocks 0..=10 normally, then read its H_max state.
    let (baseline_state, blocks) = commit_blocks_normally(&network, max_height);
    let (baseline_utxos, baseline_pools, baseline_balances) = collect_state(&baseline_state);

    // The survivor set is exactly the baseline's H_max unspent output set, in
    // sorted on-disk order — which is what the emitter produces. With elision
    // on, every survivor's create is written and every non-survivor's is elided.
    let survivor_bytes: Vec<u8> = baseline_utxos.iter().flatten().copied().collect();
    let survivor_set = Arc::new(SurvivorSet::from_bytes(survivor_bytes).expect("survivors sorted"));

    // Snapshot-consume: a fresh state with elision on, committing the same
    // blocks. Per-block balances and value pools are skipped during the commits.
    let mut consume_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("test database opens");
    consume_state
        .db
        .set_snapshot_consume(Some(Arc::new(SnapshotConsumeState::from_parts(
            network.clone(),
            Height(max_height),
            // Elide UTXO bytes for non-survivors (the crash-safe default).
            true,
            Some(survivor_set),
        ))));

    for (height, block) in blocks.iter().enumerate() {
        consume_state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "snapshot-consume commit",
            )
            .unwrap_or_else(|error| {
                panic!("block {height} commits in snapshot-consume mode: {error:?}")
            });
    }

    // At H_max, load the verified final balances and value pools — exactly what
    // the engine does after fetching and verifying them against pinned hashes.
    let balances_to_load = baseline_state.db.all_address_balances();
    consume_state
        .db
        .bulk_load_address_balances(balances_to_load, 1024)
        .expect("bulk-load balances");
    consume_state
        .db
        .bulk_load_chain_value_pools(baseline_pools, Height(max_height))
        .expect("bulk-load value pools");

    // The snapshot-consume state at H_max must be byte-identical to the normal
    // state for the consensus artifacts.
    let (consume_utxos, consume_pools, consume_balances) = collect_state(&consume_state);

    assert_eq!(
        baseline_state.db.finalized_tip_hash(),
        consume_state.db.finalized_tip_hash(),
        "both states reach the same tip",
    );
    assert_eq!(
        baseline_utxos, consume_utxos,
        "UTXO set is byte-identical at H_max",
    );
    assert_eq!(
        baseline_pools, consume_pools,
        "chain value pools are byte-identical at H_max",
    );
    assert_eq!(
        baseline_balances, consume_balances,
        "address balances are byte-identical at H_max",
    );

    // `block_info` must exist for every committed block even in snapshot-consume
    // mode (finding #5): the per-block value-pool *derivation* is skipped, but
    // the `block_info` entry (carrying the block size) is still written, so
    // `getblock` RPC and value-pool reconstruction have it. The size must match
    // the normally-committed block at every height; the tip's value pool is
    // backfilled to the verified final pool at H_max
    // (`bulk_load_chain_value_pools`), and sub-tip value pools are placeholders
    // in consume mode (the already-accepted intermediate-RPC divergence).
    for height in 0..=max_height {
        let hash_or_height = crate::HashOrHeight::Height(Height(height));
        let baseline_info = baseline_state
            .db
            .block_info(hash_or_height)
            .unwrap_or_else(|| panic!("baseline block_info exists at height {height}"));
        let consume_info = consume_state
            .db
            .block_info(hash_or_height)
            .unwrap_or_else(|| panic!("consume block_info must exist at height {height}"));
        assert_eq!(
            baseline_info.size(),
            consume_info.size(),
            "block_info size matches at height {height}",
        );
    }

    // The tip block's `block_info` value pool is backfilled to the verified
    // final pool, so it matches the normal node at H_max.
    let tip = crate::HashOrHeight::Height(Height(max_height));
    assert_eq!(
        baseline_state
            .db
            .block_info(tip)
            .expect("baseline tip block_info")
            .value_pools(),
        consume_state
            .db
            .block_info(tip)
            .expect("consume tip block_info")
            .value_pools(),
        "tip block_info value pool is backfilled to the final pool at H_max",
    );
}

/// Anti-regression: committing checkpoint blocks through the snapshot-consume
/// write path with UTXO elision on never produces a `MissingTransparentOutput`
/// (or any other) error and never panics — the spend-value reads that used to
/// crash on an elided output are gone.
#[test]
fn snapshot_consume_elision_never_misses_a_spend() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 10;

    // The survivor set from a normal sync's H_max unspent set.
    let (baseline_state, blocks) = commit_blocks_normally(&network, max_height);
    let (baseline_utxos, _pools, _balances) = collect_state(&baseline_state);
    let survivor_bytes: Vec<u8> = baseline_utxos.iter().flatten().copied().collect();
    let survivor_set = Arc::new(SurvivorSet::from_bytes(survivor_bytes).expect("survivors sorted"));

    let mut consume_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("test database opens");
    consume_state
        .db
        .set_snapshot_consume(Some(Arc::new(SnapshotConsumeState::from_parts(
            network.clone(),
            Height(max_height),
            true,
            Some(survivor_set),
        ))));

    // Every commit must succeed: the location-only spend resolution never reads
    // a (possibly elided) spent value, so it can't miss.
    for (height, block) in blocks.iter().enumerate() {
        let result = consume_state.commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "elision anti-regression commit",
        );
        assert!(
            result.is_ok(),
            "block {height} must commit with elision on, got {result:?}",
        );
    }
}

/// Deserializes a mainnet block from its raw bytes.
fn block_from_bytes(bytes: &[u8]) -> Arc<Block> {
    bytes
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("test vector block deserializes")
}

/// Supplied trees are only accepted where the block header directly pins them
/// (the Sapling/Blossom era). For any other era the supplied trees are refused
/// (`Ok(false)`), so the caller folds instead, and a malicious peer can never
/// inject an unverified tree (finding #27). In the Sapling era a wrong supplied
/// Sapling root is rejected with a fatal error.
#[test]
fn supplied_trees_are_only_accepted_when_header_pins_them() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // An empty supplied tree set. Its Sapling root is the empty-tree root, which
    // does not match any real Sapling-era block header.
    let empty_trees = NoteCommitmentTrees::default();

    // Pre-Sapling (block 1): the header is a reserved field, not a Sapling root,
    // so supplied trees are refused regardless of their contents.
    let pre_sapling = block_from_bytes(zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_ref());
    assert!(
        !FinalizedState::supplied_trees_are_verifiable(&pre_sapling, &empty_trees, &network)
            .expect("pre-Sapling supplied trees are refused, not an error"),
        "pre-Sapling supplied trees must be refused (folded), never accepted unverified",
    );

    // Heartwood (block 903,001): the header commits to the chain-history root,
    // which does not pin this block's trees, so supplied trees are refused.
    let heartwood = block_from_bytes(zebra_test::vectors::BLOCK_MAINNET_903001_BYTES.as_ref());
    assert!(
        !FinalizedState::supplied_trees_are_verifiable(&heartwood, &empty_trees, &network)
            .expect("Heartwood supplied trees are refused, not an error"),
        "Heartwood supplied trees must be refused (folded), never accepted unverified",
    );

    // Sapling era (block 419,201): the header pins the bare Sapling root, so a
    // supplied tree whose root does not match is rejected with a fatal error
    // (this is the injection guard the empty tree triggers).
    let sapling = block_from_bytes(zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.as_ref());
    let result = FinalizedState::supplied_trees_are_verifiable(&sapling, &empty_trees, &network);
    let err =
        result.expect_err("a Sapling-era supplied tree with the wrong root must be a fatal error");
    // The inner error field is private, so assert on the `Debug` representation,
    // which names the `SuppliedSaplingTreeRootMismatch` variant.
    assert!(
        format!("{err:?}").contains("SuppliedSaplingTreeRootMismatch"),
        "the Sapling-era rejection must be a supplied-tree root mismatch, got {err:?}",
    );
}
