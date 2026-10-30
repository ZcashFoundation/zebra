//! Tests for transaction checks.

use std::{
    borrow::Cow,
    collections::HashSet,
    hash::{Hash, Hasher},
};

use super::check_for_duplicates;
use crate::error::TransactionError;

#[derive(Clone, Debug)]
struct Key {
    id: u8,
    payload: u8,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[test]
fn duplicate_detection_matches_hash_set_for_small_and_large_inputs() {
    for length in 0usize..=10 {
        let unique: Vec<_> = (0..length)
            .map(|index| {
                let index = u8::try_from(index).expect("test index is at most ten");
                Key {
                    id: index,
                    payload: index,
                }
            })
            .collect();

        let check = |keys: &[Key]| {
            let items = || {
                keys.iter().enumerate().map(|(index, key)| {
                    if index % 2 == 0 {
                        Cow::Borrowed(key)
                    } else {
                        Cow::Owned(key.clone())
                    }
                })
            };
            let error = |key: Key| TransactionError::Other(format!("{}:{}", key.id, key.payload));

            let mut seen = HashSet::new();
            let expected = items()
                .find_map(|item| seen.replace(item))
                .map(|prior| error(prior.into_owned()));

            let actual = check_for_duplicates(items(), error).err();
            let mut unknown_size_items = items();
            let without_upper_bound =
                check_for_duplicates(std::iter::from_fn(|| unknown_size_items.next()), error).err();
            let mut huge_size_items = items();
            let with_huge_upper_bound = check_for_duplicates(
                std::iter::from_fn(|| huge_size_items.next()).take(usize::MAX),
                error,
            )
            .err();

            // Compare the error payload, not its Display text, which omits Other's contents.
            assert_eq!(actual, expected, "keys: {keys:?}");
            assert_eq!(without_upper_bound, expected, "keys: {keys:?}");
            assert_eq!(with_huge_upper_bound, expected, "keys: {keys:?}");
        };

        check(&unique);
        for first in 0..length {
            for second in first + 1..length {
                let mut duplicate = unique.clone();
                duplicate[second].id = duplicate[first].id;
                duplicate[second].payload =
                    200 + u8::try_from(second).expect("test index is at most ten");
                check(&duplicate);
            }
        }
    }
}
