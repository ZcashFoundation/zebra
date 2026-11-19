//! The block write task must not read the finalized tip from the database to validate a block on
//! a non-finalized parent.
//!
//! The reads are counted on the validating thread, which makes the decision this checks. A block
//! with note commitments runs some of its other checks on rayon threads.

use std::cell::Cell;

use zebra_chain::serialization::ZcashDeserializeInto;

use crate::service::finalized_state::FINALIZED_TIP_READS;

use super::{prepared::root_state, *};

/// Returns the output of `run`, and the number of finalized tip reads it made on this thread.
fn tip_reads<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = FINALIZED_TIP_READS.with(Cell::get);
    let output = run();

    (output, FINALIZED_TIP_READS.with(Cell::get) - before)
}

#[test]
fn only_a_block_without_a_non_finalized_parent_reads_the_finalized_tip() {
    let _init_guard = zebra_test::init();

    // The parent is in a chain, so the block extends that chain without a tip read.
    let (finalized, mut state, root) = root_state();
    let child = root.make_fake_child();
    let (validated, reads) =
        tip_reads(|| validate_non_finalized(&finalized.db, &state, child.clone().prepare(), None));
    assert_eq!(reads, 0);
    state.insert_validated(validated.expect("a child of the chain tip is valid").0);
    assert_eq!(state.chain_count(), 1);
    assert_eq!(state.best_tip().unwrap().1, child.hash());

    // A block committed with its proposal's verdict extends that chain without a tip read too.
    let grandchild = child.make_fake_child();
    let mut validated = state.clone();
    validated
        .commit_block(grandchild.clone().prepare(), &finalized.db)
        .unwrap();
    let verdict = validated.best_chain().unwrap().tip_block().unwrap().clone();
    let (committed, reads) =
        tip_reads(|| state.commit_prevalidated_contextual(verdict, &finalized.db, None));
    assert_eq!(reads, 0);
    committed.expect("a block with a verdict for the chain tip is valid");
    assert_eq!(state.best_tip().unwrap().1, grandchild.hash());

    // No chain has the parent, so only the database can tell that it is the finalized tip.
    let (finalized, mut state, _genesis) = new_state_with_mainnet_genesis();
    let root: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let (validated, reads) =
        tip_reads(|| validate_non_finalized(&finalized.db, &state, root.clone().prepare(), None));
    assert!(reads > 0);
    state.insert_validated(validated.expect("a child of the finalized tip is valid").0);
    assert_eq!(state.chain_count(), 1);
    assert_eq!(state.best_tip().unwrap().1, root.hash());

    // A block whose parent is in neither state still can't be validated.
    let orphan = root.make_fake_child().make_fake_child();
    let (validated, _reads) =
        tip_reads(|| validate_non_finalized(&finalized.db, &state, orphan.prepare(), None));
    assert!(matches!(
        validated,
        Err(ValidateContextError::NotReadyToBeCommitted)
    ));
}
