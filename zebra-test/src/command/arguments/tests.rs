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

    /// Test that arguments in an [`Arguments`] instance can be overridden.
    ///
    /// Generate a list of arguments to add and a list of overrides for those arguments. Also
    /// generate a list of extra arguments.
    ///
    /// All arguments from the three lists are added to an [`Arguments`] instance. The generated
    /// list of strings from the [`Arguments::into_arguments`] method is compared to a list of
    /// `expected_strings`.
    ///
    /// To build the list of `expected_strings`, a new `overridden_list` is compiled from the three
    /// original lists. The list is compiled by manually handling overrides in a compatible way that
    /// keeps the argument order when overriding and adding new arguments to the end of the list.
    #[test]
    fn arguments_can_be_overridden(
        (argument_list, override_list) in Argument::list_and_overrides_strategy(),
        extra_arguments in Argument::list_strategy(),
    ) {
        let arguments_to_add: Vec<_> = argument_list
            .into_iter()
            .chain(override_list)
            .chain(extra_arguments)
            .collect();

        let arguments = collect_arguments(arguments_to_add.clone());
        let argument_strings: Vec<_> = arguments.into_arguments().collect();

        let overridden_list = handle_overrides(arguments_to_add);
        let expected_strings = expand_arguments(overridden_list);

        assert_eq!(argument_strings, expected_strings);
    }

    /// Test that arguments in an [`Arguments`] instance can be merged.
    ///
    /// Generate a list of arguments to add and a list of overrides for those arguments. Also
    /// generate a list of extra arguments.
    ///
    /// The first list is added to a first [`Arguments`] instance, while the second and third lists
    /// are added to a second [`Arguments`] instance. The second instance is then merged into the
    /// first instance. The generated list of strings from the [`Arguments::into_arguments`] method
    /// of that first instance is compared to a list of `expected_strings`, which is built exactly
    /// like in the [`arguments_can_be_overridden`] test.
    #[test]
    fn arguments_can_be_merged(
        (argument_list, override_list) in Argument::list_and_overrides_strategy(),
        extra_arguments in Argument::list_strategy(),
    ) {
        let all_arguments: Vec<_> = argument_list
            .clone()
            .into_iter()
            .chain(override_list.clone())
            .chain(extra_arguments.clone())
            .collect();

        let arguments_for_second_instance = override_list.into_iter().chain(extra_arguments).collect();

        let mut first_arguments = collect_arguments(argument_list);
        let second_arguments = collect_arguments(arguments_for_second_instance);

        first_arguments.merge_with(second_arguments);

        let argument_strings: Vec<_> = first_arguments.into_arguments().collect();

        let overridden_list = handle_overrides(all_arguments);
        let expected_strings = expand_arguments(overridden_list);

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

/// Processes a list of [`Argument`] items handling overrides, returning a new list of [`Argument`]
/// items without duplicate arguments.
///
/// This follows the behavior of [`Arguments`] when handling overrides, so that the returned list
/// is equivalent to an [`Arguments`] instance built from the same input list.
fn handle_overrides(argument_list: Vec<Argument>) -> Vec<Argument> {
    let mut overridden_list = Vec::new();

    for override_argument in argument_list {
        let search_term = match &override_argument {
            Argument::LoneArgument(argument) => argument,
            Argument::KeyValuePair(key, _value) => key,
        };

        let argument_to_override =
            overridden_list
                .iter_mut()
                .find(|existing_argument| match existing_argument {
                    Argument::LoneArgument(argument) => argument == search_term,
                    Argument::KeyValuePair(key, _value) => key == search_term,
                });

        if let Some(argument_to_override) = argument_to_override {
            *argument_to_override = override_argument;
        } else {
            overridden_list.push(override_argument);
        }
    }

    overridden_list
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

    /// Generates a list of unique arbitrary [`Argument`] instances and a list of [`Argument`]
    /// instances that override arguments from the first list.
    pub fn list_and_overrides_strategy() -> impl Strategy<Value = (Vec<Argument>, Vec<Argument>)> {
        // Generate a list of unique arbitrary arguments.
        Self::list_strategy()
            // Generate a list of arguments to override (referenced by indices) with new arbitrary
            // random strings.
            .prop_flat_map(|argument_list| {
                let argument_list_count = argument_list.len();

                let max_overrides = argument_list_count * 3;
                let min_overrides = 1.min(max_overrides);

                let override_strategy = (0..argument_list_count, "\\PC*");

                (
                    Just(argument_list),
                    hash_set(override_strategy, min_overrides..=max_overrides),
                )
            })
            // Transform the override references and random strings into [`Argument`] instances,
            // with empty strings leading to the creation of lone arguments.
            .prop_map(|(argument_list, override_seeds)| {
                let override_list = override_seeds
                    .into_iter()
                    .map(|(index, new_value)| {
                        let key = match &argument_list[index] {
                            Argument::LoneArgument(argument) => argument,
                            Argument::KeyValuePair(key, _value) => key,
                        }
                        .clone();

                        if new_value.is_empty() {
                            Argument::LoneArgument(key)
                        } else {
                            Argument::KeyValuePair(key, new_value)
                        }
                    })
                    .collect();

                (argument_list, override_list)
            })
    }
}
