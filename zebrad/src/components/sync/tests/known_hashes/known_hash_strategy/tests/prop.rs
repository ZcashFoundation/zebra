//! Properties of bounded known-hash generation and shrinking.

use proptest::{collection::vec, prelude::*, strategy::ValueTree as _, test_runner::TestRunner};
use regex::Regex;

use super::super::{segment_lengths, KnownHashStrategy};

proptest! {
    /// Generated and shrunk flags keep matching their pattern and total length bound.
    #[test]
    fn membership_patterns_survive_shrinking(
        tokens in vec(prop_oneof![Just("u"), Just("K"), Just(".*")], 0..=8),
        extra_length in 0usize..=10,
        backtrack in vec(any::<bool>(), 1..=64),
    ) {
        let _test_guard = zebra_test::init();

        let pattern = tokens.concat();
        let min_length = tokens.iter().filter(|token| **token != ".*").count();
        let max_length = min_length + extra_length;
        let matcher = Regex::new(&format!("^{}$", pattern.replace(".*", "[uK]*"))).unwrap();
        let strategy = KnownHashStrategy::new(&pattern, max_length);
        let mut runner = TestRunner::deterministic();
        let mut tree = strategy.new_tree(&mut runner).unwrap();
        let mut can_complicate = false;

        for backtrack in backtrack {
            let flags = tree.current();
            let text = String::from_iter(
                flags.iter().map(|is_known| if *is_known { 'K' } else { 'u' }),
            );

            prop_assert!((min_length..=max_length).contains(&flags.len()));
            prop_assert!(matcher.is_match(&text), "{text} does not match {pattern}");

            can_complicate = if backtrack && can_complicate {
                tree.complicate()
            } else {
                tree.simplify()
            };
        }
    }

    /// Segment allocation preserves the exact budget and the number of wildcards.
    #[test]
    fn wildcard_segments_partition_the_budget(
        (total, count, lengths) in (0usize..=100, 1usize..=8)
            .prop_flat_map(|(total, count)| {
                (Just(total), Just(count), segment_lengths(total, count))
            }),
    ) {
        let _test_guard = zebra_test::init();

        prop_assert_eq!(lengths.len(), count);
        prop_assert_eq!(lengths.iter().sum::<usize>(), total);
    }
}
