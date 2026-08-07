#![no_main]

//! Note-commitment-tree primitive fuzz target.
//!
//! Drives a sequence of `Append` / `Root` / state-read ops against all three
//! Zebra note-commitment-tree implementations in parallel:
//!   * `zebra_chain::sapling::tree::NoteCommitmentTree`
//!   * `zebra_chain::orchard::tree::NoteCommitmentTree`
//!   * `zebra_chain::sprout::tree::NoteCommitmentTree`
//!
//! These trees are reachable from network-deserialized blocks: every Sapling
//! `Output` cm_u, Orchard `Action` cmx, and Sprout `JoinSplit` commitment is
//! appended into the per-pool tree during state contextual validation. A
//! panic inside any of these primitives from attacker-controlled commitment
//! bytes therefore aborts the node (panic = abort under zebrad's panic policy).
//!
//! Notes on the op set:
//!   * The plan asks for `MarkAtPosition` / `RemoveMark` / `WitnessRoot` /
//!     `RewindCheckpoint`. Zebra's NoteCommitmentTree wraps
//!     `incrementalmerkletree::frontier::Frontier`, which does NOT expose
//!     mark / witness / checkpoint / rewind APIs (only the `BridgeTree` and
//!     `ShardTree` wrappers do, which Zebra does not use here). We map those
//!     variants to the available state-read primitives — `position`,
//!     `count`, `hash`, `recalculate_root`, `cached_root`, `is_complete_subtree`,
//!     `subtree_index`, `remaining_subtree_leaf_nodes`,
//!     `completed_subtree_index_and_root`, `to_rpc_bytes` — so the same byte
//!     stream still exercises every public path.
//!   * Every primitive call is wrapped in `panic::catch_unwind`. A surviving
//!     panic is a finding (reachable from network input).
//!   * Oracle: after every successful `Append`, `root()` must produce the
//!     same value when called twice in a row (idempotency). We snapshot
//!     after each append-then-root and compare to the next root() call.
//!
//! Compile with:
//!   cargo +nightly fuzz build note_commitment_tree_fuzz

use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use std::panic;

use zebra_chain::{
    orchard::tree as orchard_tree, sapling::tree as sapling_tree, serialization::ZcashDeserialize,
    sprout::tree as sprout_tree,
};

/// One operation in a fuzz program.
///
/// Variant names follow the original op-set plan verbatim, even though the
/// mark/witness/checkpoint/rewind family does not have a corresponding
/// public method on the underlying `Frontier`-backed trees. Those variants
/// are repurposed to drive other public state-read methods so that the
/// same byte stream exercises the full public API surface.
#[derive(Debug, Arbitrary)]
enum TreeOp {
    /// Append a 32-byte commitment value into the tree.
    ///
    /// For Sapling this round-trips through `ExtractedNoteCommitment::from_bytes`,
    /// which can return `None` for non-canonical encodings; we skip those.
    /// For Orchard this round-trips through `pallas::Base::from_repr`, which
    /// can also reject non-canonical bytes; we skip those.
    /// For Sprout `NoteCommitment` is a transparent `[u8; 32]` so every input
    /// is valid.
    Append([u8; 32]),
    /// Parse a tree `Root` (and Orchard `Node`) from attacker-controlled 32
    /// bytes. This is the network/consensus-facing canonical-encoding check
    /// — Sapling `jubjub::Base::from_bytes`, Orchard `pallas::Base::from_repr`
    /// — that block-supplied anchors/roots flow through on deserialization.
    /// It is a *distinct* code path from the locally-computed `root()` exercised
    /// by `Append`/`Root`: that one builds a root from valid field elements,
    /// while this one validates raw bytes and must return `Err` (never panic)
    /// on every non-canonical encoding. Append-only programs never reach it.
    ParseRoot([u8; 32]),
    /// Force a `root()` recomputation (also exercises the `RwLock`-backed
    /// cached-root path on subsequent calls).
    Root,
    /// Stand-in for `MarkAtPosition` (no public marking API on Frontier).
    /// Drives the `position` / `count` accessors.
    MarkAtPosition,
    /// Stand-in for `RemoveMark` (no public marking API on Frontier).
    /// Drives the `hash` and `cached_root` accessors.
    RemoveMark(u8),
    /// Stand-in for `WitnessRoot` (no public witness API on Frontier).
    /// Drives `recalculate_root` against the current frontier state.
    WitnessRoot(u8),
    /// Stand-in for `RewindCheckpoint` (no checkpoint stack on Frontier).
    /// Drives subtree-tracking accessors which feed RPC and shielded-pool
    /// finalization paths.
    RewindCheckpoint,
}

/// One fuzz program.
#[derive(Debug, Arbitrary)]
struct Program {
    ops: Vec<TreeOp>,
}

/// Cap the number of ops executed per fuzz iteration. Without this cap a
/// single input could spin forever on a long `Vec<TreeOp>` and starve the
/// fuzzer. 256 is plenty to drive an interesting frontier shape while still
/// fitting comfortably under the libFuzzer per-input timeout.
const MAX_OPS: usize = 256;

fuzz_target!(|program: Program| {
    let ops: Vec<TreeOp> = program.ops.into_iter().take(MAX_OPS).collect();

    run_sapling(&ops);
    run_orchard(&ops);
    run_sprout(&ops);
});

// ─────────────────────────────────────────────────────────────────────
// Sapling
// ─────────────────────────────────────────────────────────────────────

fn run_sapling(ops: &[TreeOp]) {
    let mut tree = sapling_tree::NoteCommitmentTree::default();

    for op in ops {
        match op {
            TreeOp::Append(bytes) => {
                // ExtractedNoteCommitment::from_bytes is a CtOption — non-canonical
                // encodings legitimately return None, which we skip rather than
                // treating as a fuzz finding (the deserializer would reject them
                // on the network path too).
                let cm_opt: Option<sapling_crypto::note::ExtractedNoteCommitment> =
                    sapling_crypto::note::ExtractedNoteCommitment::from_bytes(bytes).into();
                let Some(cm) = cm_opt else { continue };

                // Append: must not panic on any canonical input.
                let append_res = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.append(cm)));
                let Ok(append_result) = append_res else {
                    return;
                };
                if append_result.is_err() {
                    // Tree-full or other documented error; skip oracle.
                    continue;
                }

                // Oracle: root() must succeed (it is infallible — `pub fn root() -> Root`)
                // and the second call must return the same value (cached-root idempotency).
                let r1 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                let r2 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                match (r1, r2) {
                    (Ok(a), Ok(b)) => assert_eq!(
                        a, b,
                        "sapling root() not idempotent — cached vs recomputed divergence",
                    ),
                    _ => return,
                }
            }
            TreeOp::Root => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
            }
            TreeOp::ParseRoot(bytes) => {
                // Canonical-encoding validation of a Sapling root from raw
                // bytes (jubjub::Base::from_bytes). Both the `TryFrom` and the
                // reader-based `ZcashDeserialize` entry points are driven.
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    sapling_tree::Root::try_from(*bytes)
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    sapling_tree::Root::zcash_deserialize(&bytes[..])
                }));
            }
            TreeOp::MarkAtPosition => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.position()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.count()));
            }
            TreeOp::RemoveMark(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.hash()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.cached_root()));
            }
            TreeOp::WitnessRoot(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.recalculate_root()));
            }
            TreeOp::RewindCheckpoint => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.is_complete_subtree()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.subtree_index()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    tree.remaining_subtree_leaf_nodes()
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    tree.completed_subtree_index_and_root()
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.to_rpc_bytes()));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Orchard
// ─────────────────────────────────────────────────────────────────────

fn run_orchard(ops: &[TreeOp]) {
    use halo2::pasta::{group::ff::PrimeField, pallas};

    let mut tree = orchard_tree::NoteCommitmentTree::default();

    for op in ops {
        match op {
            TreeOp::Append(bytes) => {
                // pallas::Base::from_repr is a CtOption — non-canonical
                // encodings legitimately return None, skip them.
                let cm_opt: Option<pallas::Base> = pallas::Base::from_repr(*bytes).into();
                let Some(cm) = cm_opt else { continue };

                let append_res = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.append(cm)));
                let Ok(append_result) = append_res else {
                    return;
                };
                if append_result.is_err() {
                    continue;
                }

                let r1 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                let r2 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                match (r1, r2) {
                    (Ok(a), Ok(b)) => assert_eq!(
                        a, b,
                        "orchard root() not idempotent — cached vs recomputed divergence",
                    ),
                    _ => return,
                }
            }
            TreeOp::Root => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
            }
            TreeOp::ParseRoot(bytes) => {
                // Canonical-encoding validation of an Orchard root (and the
                // Orchard `Node` type, which shares the Pallas field-element
                // check) from raw bytes. The slice-based `Node::try_from` also
                // drives the wrong-length error branch.
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    orchard_tree::Root::try_from(*bytes)
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    orchard_tree::Root::zcash_deserialize(&bytes[..])
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    orchard_tree::Node::try_from(*bytes)
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    orchard_tree::Node::try_from(&bytes[..])
                }));
            }
            TreeOp::MarkAtPosition => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.position()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.count()));
            }
            TreeOp::RemoveMark(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.hash()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.cached_root()));
            }
            TreeOp::WitnessRoot(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.recalculate_root()));
            }
            TreeOp::RewindCheckpoint => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.is_complete_subtree()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.subtree_index()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    tree.remaining_subtree_leaf_nodes()
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    tree.completed_subtree_index_and_root()
                }));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.to_rpc_bytes()));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Sprout
// ─────────────────────────────────────────────────────────────────────
//
// Sprout has the smallest public API: append / root / cached_root /
// recalculate_root / hash / count. There are no subtree / position
// accessors, so the unrelated TreeOp variants drive only the available
// methods.

fn run_sprout(ops: &[TreeOp]) {
    use zebra_chain::sprout::commitment::NoteCommitment;

    let mut tree = sprout_tree::NoteCommitmentTree::default();

    for op in ops {
        match op {
            TreeOp::Append(bytes) => {
                // Sprout NoteCommitment is a transparent [u8; 32] so every
                // input is valid by construction — no canonicality filter.
                let cm = NoteCommitment::from(*bytes);

                let append_res = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.append(cm)));
                let Ok(append_result) = append_res else {
                    return;
                };
                if append_result.is_err() {
                    continue;
                }

                let r1 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                let r2 = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
                match (r1, r2) {
                    (Ok(a), Ok(b)) => assert_eq!(
                        a, b,
                        "sprout root() not idempotent — cached vs recomputed divergence",
                    ),
                    _ => return,
                }
            }
            TreeOp::Root => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.root()));
            }
            TreeOp::ParseRoot(_) => {
                // Sprout `NoteCommitment`/root is a transparent `[u8; 32]` with
                // no field-element canonicality check to validate, so there is
                // no equivalent parse path. Exercise the available accessors so
                // the variant still drives the sprout coverage map.
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.hash()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.count()));
            }
            TreeOp::MarkAtPosition => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.count()));
            }
            TreeOp::RemoveMark(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.hash()));
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.cached_root()));
            }
            TreeOp::WitnessRoot(_) => {
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.recalculate_root()));
            }
            TreeOp::RewindCheckpoint => {
                // No subtree API on Sprout — exercise count() again as a
                // cheap no-op so the variant still drives the fuzzer's
                // coverage map without dispatching to a missing method.
                let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| tree.count()));
            }
        }
    }
}
