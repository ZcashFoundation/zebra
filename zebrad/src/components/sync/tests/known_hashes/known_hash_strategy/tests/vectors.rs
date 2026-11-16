//! Fixed cases for known-hash pattern parsing and generation.

use std::{collections::BTreeSet, panic::catch_unwind};

use proptest::{
    strategy::{Strategy as _, ValueTree as _},
    test_runner::TestRunner,
};

use super::super::{KnownHashPattern, KnownHashStrategy, Token};

/// Wildcards share one total bound while allowing every small matching sequence.
#[test]
fn wildcards_share_the_total_length_budget() {
    let _test_guard = zebra_test::init();

    let strategy = KnownHashStrategy::new(".*u.*K.*", 4);
    let mut runner = TestRunner::deterministic();
    let mut observed = BTreeSet::new();

    for _ in 0..4096 {
        let flags = strategy.new_tree(&mut runner).unwrap().current();
        assert!(
            flags.len() <= 4,
            "wildcards must share the total length budget"
        );

        observed.insert(String::from_iter(flags.into_iter().map(|is_known| {
            if is_known {
                'K'
            } else {
                'u'
            }
        })));
    }

    let expected = BTreeSet::from_iter(
        [
            "uK", "uuK", "uKu", "uKK", "KuK", "uuuK", "uuKu", "uuKK", "uKuu", "uKuK", "uKKu",
            "uKKK", "KuuK", "KuKu", "KuKK", "KKuK",
        ]
        .map(String::from),
    );

    assert_eq!(
        observed, expected,
        "generate every bounded sequence matching .*u.*K.*"
    );
}

/// Literal patterns produce exactly their flags even when the bound allows more.
#[test]
fn literal_patterns_generate_exact_membership() {
    let _test_guard = zebra_test::init();

    let strategy = KnownHashStrategy::new("uKKu", 10);
    let mut runner = TestRunner::deterministic();
    let mut tree = strategy.new_tree(&mut runner).unwrap();

    assert_eq!(tree.current(), vec![false, true, true, false]);
    assert!(
        !tree.simplify(),
        "literal flags cannot be removed or changed"
    );
}

/// Empty patterns and wildcard-only patterns accept a zero bound without producing flags.
#[test]
fn zero_bound_generates_empty_membership() {
    let _test_guard = zebra_test::init();

    let mut runner = TestRunner::deterministic();

    for pattern in ["", ".*", ".*.*"] {
        let strategy = KnownHashStrategy::new(pattern, 0);
        let tree = strategy.new_tree(&mut runner).unwrap();

        assert!(
            tree.current().is_empty(),
            "{pattern:?} must fit the zero bound"
        );
    }
}

/// Bounds smaller than the mandatory literal count are rejected at construction.
#[test]
#[should_panic(expected = "maximum length is shorter than the pattern's literals")]
fn rejects_impossible_membership_length() {
    let _test_guard = zebra_test::init();

    KnownHashStrategy::new("uK.*", 1);
}

/// Parsing preserves literals and adjacent wildcards in their original order.
#[test]
fn parses_membership_pattern_tokens() {
    let _test_guard = zebra_test::init();

    let pattern = KnownHashPattern::parse("uK.*.*Ku");

    assert_eq!(
        pattern.tokens,
        vec![
            Token::Unknown,
            Token::Known,
            Token::AnySequence,
            Token::AnySequence,
            Token::Known,
            Token::Unknown,
        ],
    );
}

/// Unsupported characters and incomplete wildcards are rejected rather than ignored.
#[test]
fn rejects_invalid_membership_patterns() {
    let _test_guard = zebra_test::init();

    for pattern in [".", "*", ".u", "uKx", ".**", "u|K", "u K", "é"] {
        assert!(
            catch_unwind(|| KnownHashPattern::parse(pattern)).is_err(),
            "invalid pattern {pattern:?} must be rejected",
        );
    }
}
