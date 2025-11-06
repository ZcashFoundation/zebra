//! Serialization constraint helpers.

use std::ops::Deref;

use crate::serialization::SerializationError;

/// A `Vec<T>` wrapper that enforces a **minimum** and **maximum length**.
///
/// `Bounded<T, MIN, MAX>` ensures that:
/// - The number of elements is at least `MIN`, and
/// - The number of elements is at most `MAX`.
///
/// You can initialize `Bounded` from slices, arrays, or `Vec`s using `TryFrom`:
/// ```
/// # use zebra_chain::serialization::{Bounded, SerializationError};
/// # use std::convert::{TryFrom, TryInto};
/// #
/// let v: Bounded<u32, 1, 5> = vec![1, 2, 3].try_into()?;
/// assert_eq!(v.as_slice(), &[1, 2, 3]);
///
/// let v: Bounded<u32, 2, 4> = [10, 20].as_slice().try_into()?;
/// assert_eq!(v.as_slice(), &[10, 20]);
///
/// let v: Bounded<u32, 1, 3> = [42].try_into()?;
/// assert_eq!(v.as_slice(), &[42]);
///
/// let v = Bounded::<u32, 1, 3>::try_from(&[42])?;
/// assert_eq!(v.as_slice(), &[42]);
/// #
/// # Ok::<(), SerializationError>(())
/// ```
///
/// And access the inner vector via [deref coercion](https://doc.rust-lang.org/std/ops/trait.Deref.html#more-on-deref-coercion),
/// an explicit conversion, or as a slice:
/// ```
/// # use zebra_chain::serialization::Bounded;
/// # use std::convert::TryInto;
/// #
/// # let v: Bounded<u32, 4, 5> = vec![2, 3, 5, 7, 11].try_into().unwrap();
///
/// let first = v.iter().next().expect("Bounded always has a first element");
/// assert_eq!(*first, 2);
///
/// let s = v.as_slice();
/// # assert_eq!(s, [2, 3, 5, 7, 11]);
///
/// let vec: Vec<u32> = v.into_vec();
/// # assert_eq!(vec, vec![2, 3, 5, 7, 11]);
/// ```
/// 
/// `Bounded` also re-implements some slice methods with different return
/// types, to avoid redundant unwraps:
/// ```
/// # use zebra_chain::serialization::Bounded;
/// # use std::convert::TryInto;
/// #
/// # let v: Bounded<u32, 1, 5> = vec![7, 8, 9].try_into().unwrap();
///
/// let first = v.first();
/// assert_eq!(*first, 7);
///
/// let (first, rest) = v.split_first();
/// assert_eq!(*first, 7);
/// assert_eq!(rest, &[8, 9]);
/// ```
///
/// **Push behavior:**  
/// `push` returns an error if it would exceed `MAX` length:
/// ```
/// # use zebra_chain::serialization::{Bounded, SerializationError};
/// #
/// # let mut v: Bounded<u32, 1, 2> = vec![1].try_into().unwrap();
/// assert!(v.push(2).is_ok());
/// assert!(v.push(3).is_err());
/// ```
///
/// Unlike `Vec`, `first()` and `split_first()` never panic because the
/// collection always contains at least `MIN` elements.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Bounded<T, const MIN: usize, const MAX: usize> {
    /// The inner vector. Private to ensure bounds are never violated.
    inner: Vec<T>,
}

// CORRECTNESS
//
// All conversions to `Bounded<T, MIN, MAX>` must go through `TryFrom<Vec<T>>`,
// so that the type constraint is satisfied.

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for Bounded<T, MIN, MAX> {
    type Error = SerializationError;

    fn try_from(vec: Vec<T>) -> Result<Self, Self::Error> {
        let len = vec.len();
        if len < MIN {
            return Err(SerializationError::Parse("too few elements"));
        }
        if len > MAX {
            return Err(SerializationError::Parse("too many elements"));
        }

        Ok(Bounded { inner: vec })
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<&Vec<T>> for Bounded<T, MIN, MAX>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(vec: &Vec<T>) -> Result<Self, Self::Error> {
        let len = vec.len();
        if len < MIN {
            return Err(SerializationError::Parse("too few elements"));
        }
        if len > MAX {
            return Err(SerializationError::Parse("too many elements"));
        }

        Ok(Bounded {
            inner: vec.to_vec(),
        })
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<&[T]> for Bounded<T, MIN, MAX>
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
impl<T, const MIN: usize, const MAX: usize, const N: usize> TryFrom<[T; N]> for Bounded<T, MIN, MAX>
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
impl<T, const MIN: usize, const MAX: usize, const N: usize> TryFrom<&[T; N]>
    for Bounded<T, MIN, MAX>
where
    T: Clone,
{
    type Error = SerializationError;

    fn try_from(slice: &[T; N]) -> Result<Self, Self::Error> {
        slice.to_vec().try_into()
    }
}

// Deref and AsRef (but not DerefMut or AsMut, because that could break the constraint)

impl<T, const MIN: usize, const MAX: usize> Deref for Bounded<T, MIN, MAX> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<[T]> for Bounded<T, MIN, MAX> {
    fn as_ref(&self) -> &[T] {
        self.inner.as_ref()
    }
}

// Allow converting back to Vec<T>

impl<T, const MIN: usize, const MAX: usize> From<Bounded<T, MIN, MAX>> for Vec<T> {
    fn from(bv: Bounded<T, MIN, MAX>) -> Self {
        bv.inner.into()
    }
}

// `IntoIterator` for `T` and `&mut T`, because iterators can't remove items

impl<T, const MIN: usize, const MAX: usize> IntoIterator for Bounded<T, MIN, MAX> {
    type Item = T;

    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> std::vec::IntoIter<T> {
        self.inner.into_iter()
    }
}

impl<T, const MIN: usize, const MAX: usize> Bounded<T, MIN, MAX> {
    /// Returns an iterator that allows modifying each value.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.inner.iter_mut()
    }
}

impl<T, const MIN: usize, const MAX: usize> Bounded<T, MIN, MAX> {
    /// Returns a reference to the inner vector.
    pub fn as_vec(&self) -> &Vec<T> {
        &self.inner
    }

    /// Converts `self` into a vector without clones or allocation.
    ///
    /// The resulting vector can be converted back into `Bounded` via `try_into`.
    pub fn into_vec(self) -> Vec<T> {
        self.inner
    }

    /// Returns the first element.
    ///
    /// Unlike `Vec` or slice, `Bounded` always has a first element.
    pub fn first(&self) -> &T {
        &self.inner[0]
    }

    /// Returns a mutable reference to the first element.
    ///
    /// Unlike `Vec` or slice, `Bounded` always has a first element.
    pub fn first_mut(&mut self) -> &mut T {
        &mut self.inner[0]
    }

    /// Attempts to append an element to the back of the collection.
    ///
    /// Returns an error if appending the element would exceed the maximum length `MAX`.
    pub fn push(&mut self, element: T) -> Result<(), SerializationError> {
        if self.inner.len() >= MAX {
            return Err(SerializationError::Parse("too many elements"));
        }
        self.inner.push(element);
        Ok(())
    }

    /// Returns the first and all the rest of the elements of the vector.
    ///
    /// Unlike `Vec` or slice, `Bounded` always has a first element.
    pub fn split_first(&self) -> (&T, &[T]) {
        (&self.inner[0], &self.inner[1..])
    }
}

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
    use super::{AtLeastOne, Bounded};

    #[test]
    fn at_least_one_count_form_works() {
        let v: AtLeastOne<i32> = at_least_one![42; 1];
        assert_eq!(v.as_slice(), [42]);

        let v2: AtLeastOne<u8> = at_least_one![5; 2];
        assert_eq!(v2.as_slice(), [5, 5]);
    }

    #[test]
    fn bounded_vec_enforces_min_and_max() {
        // MIN = 2, MAX = 4
        type B = Bounded<u8, 2, 4>;

        // too few elements
        let too_few: Result<B, _> = vec![1].try_into();
        assert!(too_few.is_err());

        // valid length
        let ok: B = vec![1, 2].try_into().unwrap();
        assert_eq!(ok.len(), 2);

        // max length
        let max_ok: B = vec![1, 2, 3, 4].try_into().unwrap();
        assert_eq!(max_ok.len(), 4);

        // too many elements
        let too_many: Result<B, _> = vec![1, 2, 3, 4, 5].try_into();
        assert!(too_many.is_err());
    }

    #[test]
    fn iter_mut_and_into_iter_work() {
        type B = Bounded<u8, 1, 3>;
        let mut bv: B = vec![1, 2].try_into().unwrap();

        // iter_mut modifies elements
        for x in bv.iter_mut() {
            *x += 1;
        }
        assert_eq!(bv.as_vec(), &vec![2, 3]);

        // IntoIterator consumes Bounded
        let items: Vec<_> = bv.into_iter().collect();
        assert_eq!(items, vec![2, 3]);
    }
}
