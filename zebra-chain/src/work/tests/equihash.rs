//! Cancellation checks for Zebra's wrapper around the released Equihash solver.

#[cfg(feature = "internal-miner")]
#[test]
fn equihash_solver_cancels_before_work_and_between_nonce_attempts() {
    use crate::{
        block::Block,
        serialization::ZcashDeserialize,
        work::equihash::{Solution, SolverCancelled},
    };

    let _init_guard = zebra_test::init();
    let block = Block::zcash_deserialize(zebra_test::vectors::BLOCKS[0]).unwrap();

    // Check 1 cancels before calling the library, check 2 cancels its first nonce request, and
    // check 3 cancels after one complete nonce attempt: either on the next nonce request or on
    // Zebra's post-solve check if that attempt found solutions. No digit-round callback exists.
    for cancel_at in [1, 2, 3] {
        let mut checks = 0;
        let result = Solution::solve(*block.header, || {
            checks += 1;
            // Return an error only once: callback cancellation must not be lost or retried even
            // if a subsequent check would return Ok.
            if checks == cancel_at {
                Err(SolverCancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(SolverCancelled)));
        assert_eq!(checks, cancel_at);
    }
}
