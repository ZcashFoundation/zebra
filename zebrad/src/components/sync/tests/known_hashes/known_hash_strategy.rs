//! Bounded strategies for patterns of known and unknown block hashes.

use proptest::{
    arbitrary::any,
    collection::vec,
    strategy::{BoxedStrategy, Just, NewTree, Strategy},
    test_runner::TestRunner,
};

mod tests;

/// Generates membership flags, with `true` representing a known block hash.
#[derive(Debug)]
pub(super) struct KnownHashStrategy {
    strategy: BoxedStrategy<Vec<bool>>,
}

impl KnownHashStrategy {
    /// Generates flags matching `pattern`, with at most `max_length` flags in total.
    ///
    /// `u` generates an unknown flag, `K` a known flag, and `.*` any sequence
    /// of either. Wildcards may be empty; literals determine the minimum length.
    ///
    /// # Panics
    ///
    /// Panics for unsupported syntax or if the literals exceed `max_length`.
    pub(super) fn new(pattern: &str, max_length: usize) -> Self {
        let pattern = KnownHashPattern::parse(pattern);
        let flags = Vec::from_iter(pattern.tokens.iter().filter_map(|token| match token {
            Token::Unknown => Some(false),
            Token::Known => Some(true),
            Token::AnySequence => None,
        }));

        assert!(
            flags.len() <= max_length,
            "maximum length is shorter than the pattern's literals",
        );

        let literal_count = flags.len();
        let wildcard_count = pattern.tokens.len() - literal_count;
        let strategy = if wildcard_count == 0 {
            Just(flags).boxed()
        } else {
            (0..=max_length - literal_count)
                .prop_flat_map(move |extra_length| {
                    let tokens = pattern.tokens.clone();

                    (
                        segment_lengths(extra_length, wildcard_count),
                        vec(any::<bool>(), extra_length),
                    )
                        .prop_map(move |(lengths, flags)| {
                            let mut lengths = lengths.into_iter();
                            let mut flags = flags.into_iter();
                            let mut result = Vec::with_capacity(literal_count + extra_length);

                            for token in &tokens {
                                match token {
                                    Token::Unknown => result.push(false),
                                    Token::Known => result.push(true),
                                    Token::AnySequence => {
                                        let length = lengths.next().expect(
                                            "each wildcard has an allocated segment length",
                                        );
                                        result.extend(flags.by_ref().take(length));
                                    }
                                }
                            }

                            result
                        })
                })
                .boxed()
        };

        Self { strategy }
    }
}

impl Strategy for KnownHashStrategy {
    type Tree = <BoxedStrategy<Vec<bool>> as Strategy>::Tree;
    type Value = Vec<bool>;

    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        self.strategy.new_tree(runner)
    }
}

/// A parsed sequence of known-hash pattern tokens.
#[derive(Debug)]
struct KnownHashPattern {
    tokens: Vec<Token>,
}

impl KnownHashPattern {
    /// Parses `u`, `K`, and `.*` as unknown, known, and arbitrary sequences.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` contains any other syntax.
    fn parse(pattern: &str) -> Self {
        let mut characters = pattern.chars();
        let mut tokens = Vec::new();

        while let Some(character) = characters.next() {
            tokens.push(match character {
                'u' => Token::Unknown,
                'K' => Token::Known,
                '.' if characters.next() == Some('*') => Token::AnySequence,
                _ => panic!("invalid known-hash pattern {pattern:?}: expected u, K, or .*"),
            });
        }

        Self { tokens }
    }
}

/// A literal membership state or a variable-length sequence of states.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Unknown,
    Known,
    AnySequence,
}

/// Splits `total` flags among a nonzero number of segments, allowing empty segments.
///
/// # Panics
///
/// Panics if `segment_count` is zero.
fn segment_lengths(total: usize, segment_count: usize) -> impl Strategy<Value = Vec<usize>> {
    assert!(
        segment_count > 0,
        "at least one segment is needed to allocate flags"
    );

    vec(0..=total, segment_count - 1).prop_map(move |mut cuts| {
        cuts.sort_unstable();
        cuts.push(total);

        // Repeated cut positions allow zero-length segments, including at either end.
        let mut previous = 0;
        Vec::from_iter(cuts.into_iter().map(|end| {
            let length = end - previous;
            previous = end;
            length
        }))
    })
}
