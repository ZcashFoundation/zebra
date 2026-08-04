//! Property tests for synchronization response feedback.

use proptest::{collection::hash_set, prelude::*};
use zebra_chain::block::Hash;

use super::TestScenario;

proptest! {
    /// A non-empty response containing only unknown hashes receives useful feedback.
    #[test]
    fn obtain_response_with_unknown_hash_reports_useful_feedback(
        hashes in hash_set(any::<[u8; 32]>(), 1..=10),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = hashes.into_iter().map(Hash).collect();

            let mut observer = test.hashes_for_obtain_tips(hashes).await;

            assert_eq!(observer.try_outcome(), Ok(Some(true)));
        });
    }
}
