use proptest::{
    collection::{hash_set, vec},
    prelude::*,
};

use super::Arguments;

proptest! {
    /// Test that the argument order is preserved when building an [`Arguments`] instance.
    ///
    /// Check that the [`Arguments::into_arguments`] method creates a list of strings in the same
    /// order as if each argument was individually added to a list.
    #[test]
    fn argument_order_is_preserved(argument_list in Argument::list_strategy()) {
        let arguments = collect_arguments(argument_list.clone());
        let argument_strings: Vec<_> = arguments.into_arguments().collect();

        let expected_strings = expand_arguments(argument_list);

        assert_eq!(argument_strings, expected_strings);
    }
}

/// Collects a list of [`Argument`] items into an [`Arguments`] instance.
fn collect_arguments(argument_list: Vec<Argument>) -> Arguments {
    let mut arguments = Arguments::new();

    for argument in argument_list {
        match argument {
            Argument::LoneArgument(argument) => arguments.set_argument(argument),
            Argument::KeyValuePair(key, value) => arguments.set_parameter(key, value),
        }
    }

    arguments
}

/// Expands a list of [`Argument`] items into a list of [`String`]s.
///
/// This list is the list of expected strings that is expected to be generated from an [`Arguments`]
/// instance built from the same list of [`Argument`] items.
fn expand_arguments(argument_list: Vec<Argument>) -> Vec<String> {
    let mut expected_strings = Vec::new();

    for argument in argument_list {
        match argument {
            Argument::LoneArgument(argument) => expected_strings.push(argument),
            Argument::KeyValuePair(key, value) => expected_strings.extend([key, value]),
        }
    }

    expected_strings
}

/// A helper type to generate argument items.
#[derive(Clone, Debug)]
pub enum Argument {
    /// A lone argument, like for example an individual item or a flag.
    LoneArgument(String),

    /// A key value pair, usually a parameter to be configured.
    KeyValuePair(String, String),
}

impl Argument {
    /// Generates a list of unique arbitrary [`Argument`] instances.
    ///
    /// # Implementation
    ///
    /// 1. Generate a list with less than ten random strings. Then proceed by selecting which strings
    /// will become key value pairs, and generate a new random string for each value that needs to
    /// be paired to an argument key.
    pub fn list_strategy() -> impl Strategy<Value = Vec<Argument>> {
        // Generate a list with less than ten unique random strings.
        hash_set("\\PC+", 0..10)
            .prop_map(|keys| keys.into_iter().collect::<Vec<_>>())
            .prop_shuffle()
            // Select some (or none) of the keys to become key-value pairs.
            .prop_flat_map(|keys| {
                let key_count = keys.len();

                (Just(keys), vec(any::<bool>(), key_count))
            })
            // Generate random strings for the values to be paired with keys.
            .prop_flat_map(|(keys, is_pair)| {
                let value_count = is_pair.iter().filter(|&&is_pair| is_pair).count();

                (Just(keys), Just(is_pair), vec("\\PC+", value_count))
            })
            // Pair the selected keys with values, and make the non-selected keys lone arguments.
            .prop_map(|(keys, is_pair, mut values)| {
                keys.into_iter()
                    .zip(is_pair)
                    .map(|(key, is_pair)| {
                        if is_pair {
                            Argument::KeyValuePair(
                                key,
                                values.pop().expect("missing value from generated list"),
                            )
                        } else {
                            Argument::LoneArgument(key)
                        }
                    })
                    .collect()
            })
    }
}
