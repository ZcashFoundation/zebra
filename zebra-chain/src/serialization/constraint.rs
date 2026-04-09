//! Serialization constraint helpers.

use bounded_vec::BoundedVec;

/// A vector type that must contain at least one element.
///
/// This is a type alias for `BoundedVec<T, 1, { usize::MAX }>`, provided by the
/// [`bounded_vec`] crate. All functionality, including length constraints and safe
/// construction, is inherited from `BoundedVec`.
///
/// [`bounded_vec`]: https://docs.rs/bounded-vec
pub type AtLeastOne<T> = BoundedVec<T, 1, { usize::MAX }>;

/// Create an initialized [`AtLeastOne`] instance.
///
/// This macro is similar to the [`vec!`][`std::vec!`] macro, but doesn't support creating an empty
/// `AtLeastOne` instance.
///
/// # Security
///
/// This macro must only be used in tests, because it skips the `TrustedPreallocate` memory
/// denial of service checks.
#[cfg(any(test, feature = "proptest-impl"))]
#[macro_export]
macro_rules! at_least_one {
    ($element:expr; 0) => (
        compile_error!("At least one element needed to create an `AtLeastOne<T>`")
    );

    ($element:expr; $count:expr) => (
        {
            <Vec<_> as std::convert::TryInto<$crate::serialization::AtLeastOne<_>>>::try_into(
                vec![$element; $count],
            ).expect("at least one element in `AtLeastOne<_>`")
        }
    );

    ($($element:expr),+ $(,)?) => (
        {
            <Vec<_> as std::convert::TryInto<$crate::serialization::AtLeastOne<_>>>::try_into(
                vec![$($element),*],
            ).expect("at least one element in `AtLeastOne<_>`")
        }
    );
}

#[cfg(test)]
mod tests {
    use super::AtLeastOne;

    #[test]
    fn at_least_one_count_form_works() {
        let v: AtLeastOne<i32> = at_least_one![42; 1];
        assert_eq!(v.as_slice(), [42]);

        let v2: AtLeastOne<u8> = at_least_one![5; 2];
        assert_eq!(v2.as_slice(), [5, 5]);
    }
}
