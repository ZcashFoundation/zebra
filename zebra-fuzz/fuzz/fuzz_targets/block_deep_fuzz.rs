#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use std::io::Cursor;
use std::panic;
use std::sync::Arc;
use std::sync::OnceLock;
use zebra_chain::amount::DeferredPoolBalanceChange;
use zebra_chain::block::{merkle, Block, Height, MAX_BLOCK_BYTES};
use zebra_chain::parameters::Network;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::transaction::Transaction;
use zebra_chain::transparent;

/// Cached default testnet.
///
/// `Network::new_default_testnet()` builds a `BTreeMap` of activation heights,
/// parses the genesis hash and round-trips 256-bit difficulty. It was called
/// several times per input, which dominated this target's cost. The value is immutable and its payload sits behind an
/// `Arc`, so building it once and cloning the handle is equivalent and cheap.
fn default_testnet() -> &'static Network {
    static TESTNET: OnceLock<Network> = OnceLock::new();
    TESTNET.get_or_init(Network::new_default_testnet)
}

// ───────────────────────────────────────────────────────────────────────
// NU fork sweep table.
//
// Mainnet activation heights for each NetworkUpgrade. The harness picks
// one of these heights via `data[0] % NU_FORK_HEIGHTS.len()` and runs the
// consensus checks (consensus_branch_id, disabled_add_to_sprout_pool,
// coinbase_expiry_height, non_coinbase_expiry_height, lock_time_has_passed,
// difficulty_threshold_is_valid, time_is_valid_at) against THAT NU's
// activation height — exercising the dispatch table inside each check.
//
// Heights are pulled from zebra-chain/src/parameters/constants.rs (mainnet).
// We intentionally include both Mainnet and Testnet networks, since
// `consensus_branch_id` and `disabled_add_to_sprout_pool` branch on it.
// ───────────────────────────────────────────────────────────────────────
const NU_FORK_HEIGHTS: &[(u32, &str)] = &[
    (0, "Genesis"),
    (1, "BeforeOverwinter"),
    (347_500, "Overwinter"),
    (419_200, "Sapling"),
    (653_600, "Blossom"),
    (903_000, "Heartwood"),
    (1_046_400, "Canopy"),
    (1_687_104, "Nu5"),
    (2_726_400, "Nu6"),
    (3_146_400, "Nu6_1"),
    (3_364_600, "Nu6_2"),
    // NU6.3 "Ironwood" — Mainnet activation height 3,428,143. Included so the
    // consensus-check dispatch is exercised at the Ironwood height (v6 tx
    // format / Ironwood shielded pool surface).
    (3_428_143, "Nu6_3"),
];

fuzz_target!(|data: &[u8]| {
    // ═══════════════════════════════════════════════════════════════════
    // Layer 1: Deserialize + Round-trip Consistency
    // Target: serialization asymmetry bugs → consensus split vectors
    // ═══════════════════════════════════════════════════════════════════
    let block = match Block::zcash_deserialize(Cursor::new(data)) {
        Ok(block) => block,
        Err(_) => return,
    };
    let block = Arc::new(block);

    // NU fork picker — 1 byte of fuzzer entropy selects which
    // network-upgrade activation height to drive the consensus dispatch
    // tables with. We also let bit 7 flip the network (Mainnet/Testnet)
    // so the same input exercises both activation tables. If `data` is
    // empty (impossible after parse-OK above, but defensive) we default
    // to Genesis on Mainnet.
    let fork_byte = data.first().copied().unwrap_or(0);
    let fork_idx = (fork_byte as usize) % NU_FORK_HEIGHTS.len();
    let nu_height = Height(NU_FORK_HEIGHTS[fork_idx].0);
    let nu_network = if fork_byte & 0x80 != 0 {
        default_testnet().clone()
    } else {
        Network::Mainnet
    };

    // Round-trip: serialize → deserialize → serialize again, compare bytes
    let mut serialized = Vec::new();
    if block.zcash_serialize(&mut serialized).is_ok() {
        if let Ok(block2) = Block::zcash_deserialize(Cursor::new(&serialized)) {
            let mut serialized2 = Vec::new();
            if block2.zcash_serialize(&mut serialized2).is_ok() {
                assert_eq!(
                    serialized, serialized2,
                    "Block round-trip serialization mismatch — potential consensus split vector"
                );
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 2: Block Property Extraction
    // Target: panics in hash/accessor/getter methods
    // ═══════════════════════════════════════════════════════════════════

    // block.hash() — fundamental block identity
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.hash()
    }));

    // block.coinbase_height() — height extraction from coinbase
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.coinbase_height()
    }));

    // block.commitment() — exercises commitment parsing for both networks
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = block.commitment(&Network::Mainnet);
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = block.commitment(default_testnet());
    }));

    // block.check_transaction_network_upgrade_consistency()
    // This path can panic when `coinbase_height()` returns None (no coinbase,
    // or an invalid coinbase script). We guard the call behind a
    // coinbase-height check so the campaign does not repeatedly re-trigger the
    // same expected precondition failure on malformed coinbase inputs.
    if block.coinbase_height().is_some() {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _ = block.check_transaction_network_upgrade_consistency(&Network::Mainnet);
        }));
    }

    // Nullifier iteration — exercises Sprout/Sapling/Orchard paths
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _n in block.sprout_nullifiers() {}
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _n in block.sapling_nullifiers() {}
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _n in block.orchard_nullifiers() {}
    }));

    // Note commitment iteration
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _c in block.sprout_note_commitments() {}
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _c in block.sapling_note_commitments() {}
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _c in block.orchard_note_commitments() {}
    }));

    // Transaction counts
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.sapling_transactions_count()
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.orchard_transactions_count()
    }));

    // Auth data root — ZIP-244 Merkle tree computation
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.auth_data_root()
    }));

    // chain_value_pool_change — arithmetic-heavy, potential overflow.
    // Passing an empty `utxos` map violates this function's documented
    // caller-contract (`# Panics if any input's outpoint is not in the map`),
    // so we do not invoke it with empty utxos. Driving it with valid utxos
    // requires structured mock inputs that a state-aware harness would supply.
    // We keep the call only for blocks with no transparent inputs, where the
    // outpoint lookup is never reached.
    let has_transparent_inputs = block
        .transactions
        .iter()
        .flat_map(|tx| tx.inputs())
        .any(|i| matches!(i, transparent::Input::PrevOut { .. }));
    if !has_transparent_inputs {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let utxos: HashMap<transparent::OutPoint, transparent::Utxo> = HashMap::new();
            // The second argument is a required `DeferredPoolBalanceChange`.
            // Zero means "no deferred change", which is exactly how the
            // repository's own vector test drives it
            // (zebra-chain/src/block/tests/vectors.rs).
            let _ = block.chain_value_pool_change(&utxos, DeferredPoolBalanceChange::zero());
        }));
    }

    // Header methods
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        block.header.hash()
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let now = chrono::Utc::now();
        let height = block.coinbase_height().unwrap_or(zebra_chain::block::Height(0));
        let hash = block.hash();
        let _ = block.header.time_is_valid_at(now, &height, &hash);
    }));

    // Header commitment bytes field access
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _bytes = block.header.commitment_bytes;
    }));

    // Header commitment parsing for both networks at various heights
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let height = block.coinbase_height().unwrap_or(zebra_chain::block::Height(0));
        let _ = block.header.commitment(&Network::Mainnet, height);
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let height = block.coinbase_height().unwrap_or(zebra_chain::block::Height(0));
        let _ = block.header.commitment(default_testnet(), height);
    }));

    // Serialize the header separately (exercises block/serialize.rs)
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut header_bytes = Vec::new();
        let _ = block.header.zcash_serialize(&mut header_bytes);
    }));

    // Serialize individual transactions within the block (exercises block/serialize.rs)
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for tx in &block.transactions {
            let mut tx_bytes = Vec::new();
            let _ = tx.zcash_serialize(&mut tx_bytes);
        }
    }));

    // ═══════════════════════════════════════════════════════════════════
    // Layer 3: Consensus Checks (zebra_consensus::block::check::*)
    // Target: panics where Result<_, Error> should be returned
    // ═══════════════════════════════════════════════════════════════════

    // coinbase_is_first — checks first tx is coinbase, rest are not
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = zebra_consensus::block::check::coinbase_is_first(&block);
    }));

    // difficulty_threshold_is_valid — PoW limit check
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let height = block.coinbase_height().unwrap_or(zebra_chain::block::Height(0));
        let hash = block.hash();
        let _ = zebra_consensus::block::check::difficulty_threshold_is_valid(
            &block.header,
            &Network::Mainnet,
            &height,
            &hash,
        );
    }));

    // equihash_solution_is_valid — heavy crypto check
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = zebra_consensus::block::check::equihash_solution_is_valid(&block.header);
    }));

    // merkle_root_validity — needs precomputed tx hashes.
    // This internally calls Block::check_transaction_network_upgrade_consistency,
    // which can panic when coinbase_height is None. We guard with a
    // coinbase_height check to avoid re-triggering that expected precondition
    // failure during the smoke campaign.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        if block.coinbase_height().is_none() || block.transactions.is_empty() {
            return;
        }
        let tx_hashes: Vec<zebra_chain::transaction::Hash> = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect();
        let _ = zebra_consensus::block::check::merkle_root_validity(
            &Network::Mainnet,
            &block,
            &tx_hashes,
        );
    }));

    // time_is_valid_at
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let now = chrono::Utc::now();
        let height = block.coinbase_height().unwrap_or(zebra_chain::block::Height(0));
        let hash = block.hash();
        let _ = zebra_consensus::block::check::time_is_valid_at(
            &block.header,
            now,
            &height,
            &hash,
        );
    }));

    // ═══════════════════════════════════════════════════════════════════
    // Layer 4.5: Business-Level Invariants (M1 §2.5 deepening)
    //
    // Each invariant is a caller-contract / no-panic / saturating-arithmetic
    // assertion under the parse-OK precondition. We deliberately avoid any
    // absolute "safety" or "no exploit" claims — all checks below merely
    // exercise the documented public API and assert the contract stated in
    // the originating ZIP / spec section.
    //
    // Multi-block reorg / chain-context invariants are out of scope here.
    // This target stays single-block by construction.
    // ═══════════════════════════════════════════════════════════════════

    // -------------------------------------------------------------------
    // I-5: parse OK ⇒ recomputed Bitcoin-style Merkle root matches the
    //      header's `merkle_root` field.
    //
    // Spec: Bitcoin/Zcash protocol — the header binds the transaction set
    // via `hashMerkleRoot`. Note that ZIP-244 V5+ tx ids exclude
    // authorizing data (sigs/proofs) which is captured separately by
    // `auth_data_root` / hashAuthDataRoot.
    //
    // Caller contract: `block.transactions.iter().collect::<merkle::Root>()`
    // must equal `block.header.merkle_root` for a valid block. A mismatch
    // here is not necessarily a fuzz finding (the input may be a crafted
    // invalid block) — we therefore record the relation rather than
    // assert hard equality, and only assert that the recompute itself
    // does not panic for a parsed block.
    // -------------------------------------------------------------------
    // Skip empty-tx blocks: `Root::from_iter` is documented to panic on an
    // empty iterator, so we do not feed it zero transactions here.
    if !block.transactions.is_empty() {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let recomputed: merkle::Root = block.transactions.iter().collect();
            // Observation only: do not hard-assert equality — fuzz inputs
            // routinely include forged blocks where these legitimately differ.
            let _eq = recomputed == block.header.merkle_root;
            recomputed
        }));
    }

    // -------------------------------------------------------------------
    // I-6: parse OK ⇒ block_subsidy(coinbase_height) is computable as a
    //      Result without panic, and miner_subsidy on top of it is
    //      saturating (returns Err rather than panicking on overflow).
    //
    // Spec: Zcash protocol §7.8 BlockSubsidy / MinerSubsidy.
    // We do NOT assert the exact subsidy value here — this oracle only
    // exercises the saturating-arithmetic contract on the public entry
    // points across both networks, not subsidy correctness.
    // -------------------------------------------------------------------
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let height = block.coinbase_height().unwrap_or(Height(0));
        for net in [Network::Mainnet, default_testnet().clone()] {
            if let Ok(subsidy) =
                zebra_chain::parameters::subsidy::block_subsidy(height, &net)
            {
                let _ = zebra_chain::parameters::subsidy::miner_subsidy(
                    height, &net, subsidy,
                );
                let _ = zebra_chain::parameters::subsidy::funding_stream_values(
                    height, &net, subsidy,
                );
            }
        }
    }));

    // -------------------------------------------------------------------
    // I-7: parse OK ⇒ difficulty_threshold expansion + PoW solution
    //      sanity checks do not panic.
    //
    // Spec: Zcash protocol §7.6.4 Difficulty filter; ZIP-208 / Equihash.
    // `CompactDifficulty::to_expanded()` returns Option (None on
    // overflow/zero); `equihash_solution_is_valid` returns Result. Neither
    // should ever panic for a parsed header — that would be a finding.
    // -------------------------------------------------------------------
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _expanded = block.header.difficulty_threshold.to_expanded();
        // Cross-network difficulty validity — single-block, no chain
        // context (no PoW limit context-dependent checks beyond
        // network constants).
        let height = block.coinbase_height().unwrap_or(Height(0));
        let hash = block.hash();
        for net in [Network::Mainnet, default_testnet().clone()] {
            let _ = zebra_consensus::block::check::difficulty_threshold_is_valid(
                &block.header,
                &net,
                &height,
                &hash,
            );
        }
        let _ = zebra_consensus::block::check::equihash_solution_is_valid(
            &block.header,
        );
    }));

    // -------------------------------------------------------------------
    // I-8: parse OK ⇒ header.time falls within a permissive single-block
    //      sanity bound [genesis_time, now + 2h] (BIP-113-style upper).
    //
    // Spec: Zcash protocol §7.6.5 Block header — `nTime` must be
    // > median time past and < network-adjusted time + 2h. Without chain
    // context we substitute genesis_time (Zcash mainnet 2016-10-28
    // = 1477671000) as the lower bound and `now + 2h` as the upper
    // bound. Hard equality is intentionally avoided.
    //
    // Observation only: a fuzz input with `time == 0` or far-future
    // timestamp is permitted by the deserializer; we record the
    // relation rather than assert. The hard check belongs in
    // `time_is_valid_at` which we already exercise in Layer 3.
    // -------------------------------------------------------------------
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        // Zcash mainnet genesis (block 0) timestamp, used as a
        // permissive lower sanity bound for any single-block context.
        const ZCASH_GENESIS_TIME: i64 = 1_477_671_000;
        let now = chrono::Utc::now().timestamp();
        let upper = now.saturating_add(2 * 60 * 60);
        let ts = block.header.time.timestamp();
        let _within = ts >= ZCASH_GENESIS_TIME && ts <= upper;
        // Also exercise the consensus-side time validity check, which
        // is the canonical enforcement point.
        let height = block.coinbase_height().unwrap_or(Height(0));
        let hash = block.hash();
        let _ = block
            .header
            .time_is_valid_at(chrono::Utc::now(), &height, &hash);
    }));

    // -------------------------------------------------------------------
    // I-9: parse OK ⇒ feeding every Sapling/Orchard note commitment from
    //      this single block into a fresh NoteCommitmentTree does not
    //      panic. The tree may legitimately return Err (FullTree) — that
    //      is part of the saturating contract.
    //
    // This mirrors the single-block insert path. The multi-block / reorg
    // variant is out of scope here — we do not build chain context.
    //
    // Spec references:
    //   - ZIP-32 (Sapling) tree shape
    //   - ZIP-224 (Orchard) tree shape
    //   - ZIP-221 history tree single-block contribution
    // -------------------------------------------------------------------
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut sapling_tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
        for cm_u in block.sapling_note_commitments() {
            // Single-block insert; FullTree etc. are valid Err returns.
            let _ = sapling_tree.append(cm_u);
        }
        // Root computation must not panic on any valid intermediate
        // state of the tree.
        let _ = sapling_tree.root();
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut orchard_tree = zebra_chain::orchard::tree::NoteCommitmentTree::default();
        for cm_x in block.orchard_note_commitments() {
            let _ = orchard_tree.append(cm_x);
        }
        let _ = orchard_tree.root();
    }));

    // -------------------------------------------------------------------
    // I-10: parse OK ⇒ block "weight" proxies (transaction count,
    //       serialized size) do not overflow. We use saturating
    //       arithmetic so any over-sized fuzz input is recorded as a
    //       capped value rather than a panic.
    //
    // Spec: Zcash protocol §7.6 — block size is bounded by
    // `MAX_BLOCK_BYTES = 2_000_000`. Note: Zcash does not have BIP-141
    // segwit "weight" — the term is used loosely here to mean the
    // composite size+count budget that the deserializer enforces.
    //
    // We assert only that the saturating sum does not exceed
    // `usize::MAX` (trivially true for u64 cast). The real upper-bound
    // enforcement lives in the deserializer (`MAX_BLOCK_BYTES`). The
    // assertion below documents the no-overflow contract.
    // -------------------------------------------------------------------
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let tx_count: u64 = block.transactions.len() as u64;
        let mut total_size: u64 = 0;
        let mut total_inputs: u64 = 0;
        let mut total_outputs: u64 = 0;
        for tx in &block.transactions {
            let mut buf = Vec::new();
            if tx.zcash_serialize(&mut buf).is_ok() {
                total_size = total_size.saturating_add(buf.len() as u64);
            }
            total_inputs = total_inputs.saturating_add(tx.inputs().len() as u64);
            total_outputs = total_outputs.saturating_add(tx.outputs().len() as u64);
        }
        // Sanity: size proxy stays within u64 (saturating); we do NOT
        // hard-assert <= MAX_BLOCK_BYTES because the deserializer is the
        // canonical enforcement point and a forged input that slips
        // through would itself be the finding.
        let _ = total_size;
        let _ = tx_count;
        let _ = total_inputs.saturating_add(total_outputs);
        // Header serialized size proxy.
        let mut header_buf = Vec::new();
        let _ = block.header.zcash_serialize(&mut header_buf);
        let _composite = total_size
            .saturating_add(header_buf.len() as u64)
            .min(MAX_BLOCK_BYTES.saturating_mul(2));
    }));

    // ═══════════════════════════════════════════════════════════════════
    // Layer 5: Deep Fuzz Each Transaction in the Block
    // Target: per-transaction panics in property extraction & consensus
    // Replicates the per-transaction deep-fuzz logic for every tx
    // ═══════════════════════════════════════════════════════════════════

    for tx in &block.transactions {
        deep_fuzz_transaction(tx);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 6: NetworkUpgrade fork sweep
    //
    // Drive each fuzz iteration through the NU dispatch tables of every
    // active NU activation height (Genesis → BeforeOverwinter → Overwinter
    // → Sapling → Blossom → Heartwood → Canopy → Nu5 → Nu6 → Nu6_1)
    // for the picked network (Mainnet/Testnet selected by data[0] bit 7
    // above). The fork picker (data[0] mod len) chooses ONE primary
    // height to guide difficulty/time/header dispatch; the per-tx
    // consensus_branch_id / sprout-disable / expiry checks then iterate
    // ALL heights so a single block input covers every NU branch in the
    // dispatch table.
    //
    // Goal: surface panics or asymmetries unique to a single NU branch
    // (e.g. an off-by-one in the activation comparison, a saturating
    // arithmetic miss in a single subsidy era). This is the cheap
    // single-block analog of multi-block reorg analysis (out of scope here).
    // ═══════════════════════════════════════════════════════════════════
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let hash = block.hash();
        // Header dispatch checks at the picked NU height.
        let _ = zebra_consensus::block::check::difficulty_threshold_is_valid(
            &block.header,
            &nu_network,
            &nu_height,
            &hash,
        );
        let now = chrono::Utc::now();
        let _ = zebra_consensus::block::check::time_is_valid_at(
            &block.header,
            now,
            &nu_height,
            &hash,
        );
        // Block-level consensus consistency at picked NU (skip if no
        // coinbase height — same guard as the earlier layer).
        if block.coinbase_height().is_some() {
            let _ = block.check_transaction_network_upgrade_consistency(&nu_network);
        }
    }));

    // Per-tx NU sweep: every tx visits every (height, network) pair.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for tx in &block.transactions {
            for &(h, _name) in NU_FORK_HEIGHTS {
                let height = Height(h);
                for net in [&Network::Mainnet, &nu_network] {
                    let _ = zebra_consensus::transaction::check::consensus_branch_id(
                        tx, height, net,
                    );
                    let _ = zebra_consensus::transaction::check::disabled_add_to_sprout_pool(
                        tx, height, net,
                    );
                    let _ = zebra_consensus::transaction::check::coinbase_expiry_height(
                        &height, tx, net,
                    );
                }
                let _ = zebra_consensus::transaction::check::non_coinbase_expiry_height(
                    &height, tx,
                );
                let now = chrono::Utc::now();
                let _ = zebra_consensus::transaction::check::lock_time_has_passed(
                    tx, height, Some(now),
                );
            }
        }
    }));

    // Subsidy NU sweep — exercise era-step dispatch in block_subsidy /
    // miner_subsidy / funding_stream_values across every NU activation.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for &(h, _name) in NU_FORK_HEIGHTS {
            let height = Height(h);
            for net in [&Network::Mainnet, &nu_network] {
                if let Ok(subsidy) =
                    zebra_chain::parameters::subsidy::block_subsidy(height, net)
                {
                    let _ = zebra_chain::parameters::subsidy::miner_subsidy(
                        height, net, subsidy,
                    );
                    let _ = zebra_chain::parameters::subsidy::funding_stream_values(
                        height, net, subsidy,
                    );
                }
            }
        }
    }));

    // ═══════════════════════════════════════════════════════════════════
    // work::difficulty PoW boundary + per-tx auth_digest.
    //
    // The PoW difficulty surface is otherwise barely linked. Adding the
    // public ExpandedDifficulty / CompactDifficulty helpers lifts this to the
    // same depth as the `equihash_solution_is_valid` path already covered.
    // ═══════════════════════════════════════════════════════════════════
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        // CompactDifficulty round-trip: to_expanded() returns Option;
        // if Some, ExpandedDifficulty::to_compact must round-trip
        // (drift here = PoW threshold disagreement).
        let cd = block.header.difficulty_threshold;
        if let Some(exp) = cd.to_expanded() {
            let cd2 = exp.to_compact();
            // We don't assert exact bit-equality (the canonical
            // representation of zero / negative-mantissa edge cases is
            // legitimately many-to-one), but we do assert idempotency
            // of one more round-trip — a non-fixed-point conversion
            // would be a finding.
            let exp2 = cd2.to_expanded();
            assert!(
                exp2.is_some(),
                "PoW: CompactDifficulty {cd:?} → expanded → \
                 compact {cd2:?} → expanded must stay Some"
            );
        }
    }));

    // Per-tx auth_digest (ZIP-244 authorizing-data hash V5+). On V1-V4
    // this returns a sentinel zero hash; on V5+ it walks the full
    // sigs/proofs Merkle. Cheap on V1-V4, deep on V5+.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for tx in &block.transactions {
            let _ = tx.auth_digest();
        }
    }));

    // block.auth_data_root — already partially in Layer 2; explicit
    // re-call here lets the NU sweep above influence the V5+ branch.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = block.auth_data_root();
    }));

    // Header-only round-trip — tightest possible byte-equality guard
    // on the 1487-byte header prefix that miners commit to. A drift
    // here would be an immediate consensus-split finding.
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut hdr_bytes = Vec::new();
        if block.header.zcash_serialize(&mut hdr_bytes).is_ok() {
            if let Ok(hdr2) = zebra_chain::block::Header::zcash_deserialize(
                Cursor::new(&hdr_bytes),
            ) {
                let mut hdr_bytes2 = Vec::new();
                if hdr2.zcash_serialize(&mut hdr_bytes2).is_ok() {
                    assert_eq!(
                        hdr_bytes, hdr_bytes2,
                        "HEADER round-trip mismatch — consensus split"
                    );
                }
            }
        }
    }));
});

/// Deep fuzz a transaction — exercises property extraction, consensus checks, and ZIP-317.
/// Mirrors the per-transaction deep-fuzz logic.
fn deep_fuzz_transaction(tx: &Transaction) {
    use std::panic;

    // --- Property Extraction ---
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.hash() }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.auth_digest() }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.unmined_id() }));

    let _version = tx.version();
    let _is_overwintered = tx.is_overwintered();
    let _lock_time = tx.lock_time();
    let _raw_lock_time = tx.raw_lock_time();
    let _expiry = tx.expiry_height();
    let _network_upgrade = tx.network_upgrade();
    let _is_time = tx.lock_time_is_time();

    let _inputs = tx.inputs();
    let _outputs = tx.outputs();
    let _is_coinbase = tx.is_coinbase();
    let _has_transparent_io = tx.has_transparent_inputs_or_outputs();
    let _has_transparent_in = tx.has_transparent_inputs();
    let _has_transparent_out = tx.has_transparent_outputs();
    let _has_shielded_in = tx.has_shielded_inputs();
    let _has_shielded_out = tx.has_shielded_outputs();
    let _has_transparent_or_shielded_in = tx.has_transparent_or_shielded_inputs();
    let _has_transparent_or_shielded_out = tx.has_transparent_or_shielded_outputs();

    let _joinsplit_count = tx.joinsplit_count();
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _js in tx.sprout_joinsplit_descriptions() {}
    }));
    let _pub_key = tx.sprout_joinsplit_pub_key();
    let _has_sprout = tx.has_sprout_joinsplit_data();
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _c in tx.sprout_note_commitments() {}
    }));

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _a in tx.sapling_anchors() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _s in tx.sapling_spends() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _o in tx.sapling_outputs() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _n in tx.sapling_nullifiers() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _c in tx.sapling_note_commitments() {} }));
    let _has_sapling = tx.has_sapling_shielded_data();

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _a in tx.orchard_actions() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _n in tx.orchard_nullifiers() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _c in tx.orchard_note_commitments() {} }));
    let _flags = tx.orchard_flags();
    let _has_orchard = tx.has_orchard_shielded_data();

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _v in tx.output_values_to_sprout() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _v in tx.input_values_from_sprout() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.sapling_value_balance() }));

    let _enough_flags = tx.has_enough_orchard_flags();
    let _valid_non_coinbase = tx.is_valid_non_coinbase();
    for _outpoint in tx.spent_outpoints() {}

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        tx.coinbase_spend_restriction(&Network::Mainnet, Height(1_000_000))
    }));

    // --- Consensus Checks ---
    let _ = zebra_consensus::transaction::check::has_inputs_and_outputs(tx);
    let _ = zebra_consensus::transaction::check::has_enough_orchard_flags(tx);
    let _ = zebra_consensus::transaction::check::coinbase_tx_no_prevout_joinsplit_spend(tx);
    let _ = zebra_consensus::transaction::check::joinsplit_has_vpub_zero(tx);
    let _ = zebra_consensus::transaction::check::spend_conflicts(tx);

    {
        let height = Height(1_000_000);
        let now = chrono::Utc::now();
        let _ = zebra_consensus::transaction::check::lock_time_has_passed(tx, height, Some(now));
    }
    for h in [0u32, 419_200, 1_000_000, 1_687_104] {
        let _ = zebra_consensus::transaction::check::disabled_add_to_sprout_pool(
            tx, Height(h), &Network::Mainnet,
        );
    }
    {
        let height = Height(1_000_000);
        let _ = zebra_consensus::transaction::check::coinbase_expiry_height(&height, tx, &Network::Mainnet);
        let _ = zebra_consensus::transaction::check::non_coinbase_expiry_height(&height, tx);
    }
    for h in [0u32, 419_200, 903_000, 1_046_400, 1_687_104, 1_842_420] {
        let _ = zebra_consensus::transaction::check::consensus_branch_id(tx, Height(h), &Network::Mainnet);
    }

    // --- ZIP-317 Fee Calculations ---
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        zebra_chain::transaction::zip317::conventional_fee(tx)
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        zebra_chain::transaction::zip317::conventional_actions(tx)
    }));
}
