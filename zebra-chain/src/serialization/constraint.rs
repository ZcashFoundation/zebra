//! Serialization constraint helpers.

use std::ops::Deref;

use crate::serialization::SerializationError;

/// A `Vec<T>` wrapper that ensures there is at least one `T` in the vector.
///
/// You can initialize `AtLeastOne` using:
/// ```
/// # use zebra_chain::serialization::{AtLeastOne, SerializationError};
/// # use std::convert::{TryFrom, TryInto};
/// #
/// let v: AtLeastOne<u32> = vec![42].try_into()?;
/// assert_eq!(v.as_slice(), [42]);
///
/// let v: AtLeastOne<u32> = vec![42].as_slice().try_into()?;
/// assert_eq!(v.as_slice(), [42]);
///
/// let v: AtLeastOne<u32> = [42].try_into()?;
/// assert_eq!(v.as_slice(), [42]);
///
/// let v = AtLeastOne::<u32>::try_from(&[42])?;
/// assert_eq!(v.as_slice(), [42]);
/// #
/// # Ok::<(), SerializationError>(())
/// ```
///
/// And access the inner vector via [deref coercion](https://doc.rust-lang.org/std/ops/trait.Deref.html#more-on-deref-coercion),
/// an explicit conversion, or as a slice:
/// ```
/// # use zebra_chain::serialization::AtLeastOne;
/// # use std::convert::TryInto;
/// #
/// # let v: AtLeastOne<u32> = vec![42].try_into().unwrap();
/// #
/// let first = v.iter().next().expect("AtLeastOne always has a first element");
/// assert_eq!(*first, 42);
///
/// let s = v.as_slice();
/// #
/// # assert_eq!(s, [42]);
///
/// let mut m = v.into_vec();
/// #
/// # assert_eq!(m.as_slice(), [42]);
///
/// ```
///
/// `AtLeastOne` also re-implements some slice methods with different return
/// types, to avoid redundant unwraps:
/// ```
/// # use zebra_chain::serialization::AtLeastOne;
/// # use std::convert::TryInto;
/// #
/// # let v: AtLeastOne<u32> = vec![42].try_into().unwrap();
/// #
/// let first = v.first();
/// assert_eq!(*first, 42);
///
/// let (first, rest) = v.split_first();
/// assert_eq!(*first, 42);
/// assert!(rest.is_empty());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AtLeastOne<T> {
    /// The inner vector, which must have at least one element.
    ///
    /// `inner` is private, so that it can't be modified in ways that break the
    /// type constraint.
    inner: Vec<T>,
}

// CORRECTNESS
//
// All conversions to `AtLeastOne<T>` must go through `TryFrom<Vec<T>>`,
// so that the type constraint is satisfied.

impl<T> TryFrom<Vec<T>> for AtLeastOne<T> {
    type Error = SerializationError;

    fn try_from(vec: Vec<T>) -> Result<Self, Self::Error> {
        if vec.is_empty() {
            Err(SerializationError::Parse("expected at least one item"))
        } else {
            Ok(AtLeastOne { inner: vec })
        }
    }
}

impl<T> TryFrom<&Vec<T>> for AtLeastOne<T>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(vec: &Vec<T>) -> Result<Self, Self::Error> {
        if vec.is_empty() {
            Err(SerializationError::Parse("expected at least one item"))
        } else {
            Ok(AtLeastOne {
                inner: vec.to_vec(),
            })
        }
    }
}

impl<T> TryFrom<&[T]> for AtLeastOne<T>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(slice: &[T]) -> Result<Self, Self::Error> {
        slice.to_vec().try_into()
    }
}

// TODO:
// - reject [T; 0] at compile time and impl From instead?
impl<T, const N: usize> TryFrom<[T; N]> for AtLeastOne<T>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(slice: [T; N]) -> Result<Self, Self::Error> {
        slice.to_vec().try_into()
    }
}

// TODO:
// - reject [T; 0] at compile time and impl From instead?
// - remove when std is updated so that `TryFrom<&U>` is always implemented when
//   `TryFrom<U>`
impl<T, const N: usize> TryFrom<&[T; N]> for AtLeastOne<T>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(slice: &[T; N]) -> Result<Self, Self::Error> {
        slice.to_vec().try_into()
    }
}

// Deref and AsRef (but not DerefMut or AsMut, because that could break the constraint)

impl<T> Deref for AtLeastOne<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> AsRef<[T]> for AtLeastOne<T> {
    fn as_ref(&self) -> &[T] {
        self.inner.as_ref()
    }
}

// Extracting one or more items

impl<T> From<AtLeastOne<T>> for Vec<T> {
    fn from(vec1: AtLeastOne<T>) -> Self {
        vec1.inner
    }
}

// `IntoIterator` for `T` and `&mut T`, because iterators can't remove items

impl<T> IntoIterator for AtLeastOne<T> {
    type Item = T;

    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> std::vec::IntoIter<T> {
        self.inner.into_iter()
    }
}

impl<T> AtLeastOne<T> {
    /// Returns an iterator that allows modifying each value.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.inner.iter_mut()
    }
}

impl<T> AtLeastOne<T> {
    /// Returns a new `AtLeastOne`, containing a single `item`.
    ///
    /// Skips the `TrustedPreallocate` memory denial of service checks.
    /// (`TrustedPreallocate` can not defend against a single item
    /// that causes a denial of service by itself.)
    pub fn from_one(item: T) -> AtLeastOne<T> {
        AtLeastOne { inner: vec![item] }
    }

    /// Returns a reference to the inner vector.
    pub fn as_vec(&self) -> &Vec<T> {
        &self.inner
    }

    /// Converts `self` into a vector without clones or allocation.
    ///
    /// The resulting vector can be converted back into `AtLeastOne` via `try_into`.
    pub fn into_vec(self) -> Vec<T> {
        self.inner
    }

    /// Returns the first element.
    ///
    /// Unlike `Vec` or slice, `AtLeastOne` always has a first element.
    pub fn first(&self) -> &T {
        &self.inner[0]
    }

    /// Returns a mutable reference to the first element.
    ///
    /// Unlike `Vec` or slice, `AtLeastOne` always has a first element.
    pub fn first_mut(&mut self) -> &mut T {
        &mut self.inner[0]
    }

    /// Appends an element to the back of the collection.
    pub fn push(&mut self, element: T) {
        self.inner.push(element);
    }

    /// Returns the first and all the rest of the elements of the vector.
    ///
    /// Unlike `Vec` or slice, `AtLeastOne` always has a first element.
    pub fn split_first(&self) -> (&T, &[T]) {
        (&self.inner[0], &self.inner[1..])
    }
}

// TODO: consider implementing `push`, `append`, and `Extend`,
// because adding elements can't break the constraint.

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
