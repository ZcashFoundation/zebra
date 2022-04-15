use indexmap::IndexMap;

/// Helper type to keep track of arguments for spawning a process.
///
/// Stores the arguments in order, but is aware of key-value pairs to make overriding parameters
/// more predictable.
///
/// # Notes
///
/// Repeated arguments are not supported.
#[derive(Clone, Debug, Default)]
pub struct Arguments(IndexMap<String, Option<String>>);

impl Arguments {
    /// Creates a new empty list of arguments.
    pub fn new() -> Self {
        Arguments(IndexMap::new())
    }

    /// Sets a lone `argument`.
    ///
    /// If the argument is already in the list, nothing happens.
    ///
    /// If there's a key-value pair where the key is identical to the `argument`, the value of the
    /// pair is removed and the key maintains its position.
    pub fn set_argument(&mut self, argument: impl Into<String>) {
        self.0.insert(argument.into(), None);
    }

    /// Sets a key-value pair.
    ///
    /// If there is already a pair in the list with the same `parameter` key, the existing pair
    /// keeps its position but the value is updated.
    ///
    /// If there is a lone argument that is identical to the `parameter` key, the value is inserted
    /// after it.
    pub fn set_parameter(&mut self, parameter: impl Into<String>, value: impl Into<String>) {
        self.0.insert(parameter.into(), Some(value.into()));
    }

    /// Merges two argument lists.
    ///
    /// Existing pairs have their values updated if needed, and new pairs and arguments are
    /// appended to the end of this list.
    pub fn merge_with(&mut self, extra_arguments: Arguments) {
        for (key, value) in extra_arguments.0 {
            self.0.insert(key, value);
        }
    }

    /// Converts this [`Arguments`] instance into a list of strings that can be passed when
    /// spawning a process.
    pub fn into_arguments(self) -> impl Iterator<Item = String> {
        self.0
            .into_iter()
            .flat_map(|(key, value)| Some(key).into_iter().chain(value))
    }
}
