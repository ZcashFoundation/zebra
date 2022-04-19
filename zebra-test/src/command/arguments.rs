use indexmap::IndexMap;

#[cfg(test)]
mod tests;

/// Helper type to keep track of arguments for spawning a process.
///
/// Stores the arguments in order, but is aware of key-value pairs to make overriding parameters
/// more predictable.
///
/// # Notes
///
/// Repeated arguments are not supported.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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

/// Helper macro to create a list of arguments in an [`Arguments`] instance.
///
/// Accepts items separated by commas, where an item is either:
///
/// - a lone argument obtained from an expression
/// - a key-value pair in the form `"key": value_expression`
///
/// The items are added to a new [`Arguments`] instance in order.
///
/// # Implementation details
///
/// This macro is called recursively, and can run into a macro recursion limit if the list of items
/// is very long.
///
/// The first step of the macro is to create a new scope with an `args` binding to a new empty
/// `Arguments` list. That binding is returned from the scope, so the macro's generated code is an
/// expression that creates the argument list. Once the scope is created, a `@started` tag is
/// prefixed to the recursive calls, which then individually add each item.
#[macro_export]
macro_rules! args {
    // Either an empty macro call or the end of items in a list.
    ( @started $args:ident $(,)* ) => {};

    // Handling the last (or sole) item in a list, if it's a key-value pair.
    (
        @started $args:ident
        $parameter:tt : $argument:expr $(,)*
    ) => {
        $args.set_parameter($parameter, $argument);
    };

    // Handling the last (or sole) item in a list, if it's a lone argument.
    (
        @started $args:ident
        $argument:expr $(,)*
    ) => {
        $args.set_argument($argument);
    };

    // Handling the next item in a list, if it's a key-value pair argument.
    (
        @started $args:ident
        $parameter:tt : $argument:expr
        , $( $remaining_arguments:tt )+
    ) => {
        $args.set_parameter($parameter, $argument);
        args!(@started $args $( $remaining_arguments )*)
    };

    // Handling the next item in a list, if it's a lone argument.
    (
        @started $args:ident
        $argument:expr
        , $( $remaining_arguments:tt )*
    ) => {
        $args.set_argument($argument);
        args!(@started $args $( $remaining_arguments )*)
    };

    // Start of the macro, create a scope to return an `args` binding, and populate it with a
    // recursive call to this macro. A `@started` tag is used to indicate that the scope has been
    // set up.
    ( $( $arguments:tt )* ) => {
        {
            let mut args = $crate::command::Arguments::new();
            args!(@started args $( $arguments )*);
            args
        }
    };
}
