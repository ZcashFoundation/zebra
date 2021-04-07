//! Randomised property tests for MetaAddr.

use super::{super::MetaAddr, check};

use proptest::prelude::*;

proptest! {
    /// Make sure that the sanitize function reduces time and state metadata
    /// leaks.
    #[test]
    fn sanitized_fields(entry in MetaAddr::arbitrary()) {
        zebra_test::init();

        check::sanitize_avoids_leaks(&entry);
    }
}
