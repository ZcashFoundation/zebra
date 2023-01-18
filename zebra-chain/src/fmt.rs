//! Format wrappers for Zebra

use std::{fmt, ops};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::prelude::*;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

pub mod time;

pub use time::{duration_short, humantime_milliseconds, humantime_seconds};

/// Wrapper to override `Debug`, redirecting it to only output the type's name.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TypeNameToDebug<T>(pub T);

impl<T> fmt::Debug for TypeNameToDebug<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(std::any::type_name::<T>())
    }
}

impl<T> ops::Deref for TypeNameToDebug<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> ops::DerefMut for TypeNameToDebug<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<T> for TypeNameToDebug<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

/// Wrapper to override `Debug`, redirecting it to the `Display` impl.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct DisplayToDebug<T: fmt::Display>(pub T);

impl<T: fmt::Display> fmt::Debug for DisplayToDebug<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: fmt::Display> ops::Deref for DisplayToDebug<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: fmt::Display> ops::DerefMut for DisplayToDebug<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: fmt::Display> From<T> for DisplayToDebug<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

/// Wrapper to override `Debug` to display a shorter summary of the type.
///
/// For collections and exact size iterators, it only displays the
/// collection/iterator type, the item type, and the length.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryDebug<CollectionOrIter>(pub CollectionOrIter)
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator;

impl<CollectionOrIter> fmt::Debug for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}<{}>, len={}",
            std::any::type_name::<CollectionOrIter>(),
            std::any::type_name::<<CollectionOrIter as IntoIterator>::Item>(),
            self.0.clone().into_iter().len()
        )
    }
}

impl<CollectionOrIter> ops::Deref for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    type Target = CollectionOrIter;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<CollectionOrIter> ops::DerefMut for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<CollectionOrIter> From<CollectionOrIter> for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    fn from(collection: CollectionOrIter) -> Self {
        Self(collection)
    }
}

impl<CollectionOrIter> IntoIterator for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator + Clone,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    type Item = <CollectionOrIter as IntoIterator>::Item;
    type IntoIter = <CollectionOrIter as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl<CollectionOrIter> Arbitrary for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: Arbitrary + IntoIterator + Clone + 'static,
    <CollectionOrIter as IntoIterator>::IntoIter: ExactSizeIterator,
{
    type Parameters = <CollectionOrIter as Arbitrary>::Parameters;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        CollectionOrIter::arbitrary_with(args)
            .prop_map_into()
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// Wrapper to override `Debug`, redirecting it to hex-encode the type.
/// The type must be hex-encodable.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[serde(transparent)]
pub struct HexDebug<T: AsRef<[u8]>>(pub T);

impl<T: AsRef<[u8]>> fmt::Debug for HexDebug<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(std::any::type_name::<T>())
            .field(&hex::encode(self.as_ref()))
            .finish()
    }
}

impl<T: AsRef<[u8]>> ops::Deref for HexDebug<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: AsRef<[u8]>> ops::DerefMut for HexDebug<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: AsRef<[u8]>> From<T> for HexDebug<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}
