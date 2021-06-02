//! Format wrappers for Zebra

use std::{fmt, ops};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayToDebug<T>(pub T);

impl<T> fmt::Debug for DisplayToDebug<T>
where
    T: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> ops::Deref for DisplayToDebug<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> From<T> for DisplayToDebug<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

/// Wrapper to override `Debug` to display a shorter summary of the type.
///
/// For collections and exact size iterators, it only displays the
/// collection/iterator type, the item type, and the length.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryDebug<CollectionOrIter>(pub CollectionOrIter);

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

impl<CollectionOrIter> ops::Deref for SummaryDebug<CollectionOrIter> {
    type Target = CollectionOrIter;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<CollectionOrIter> From<CollectionOrIter> for SummaryDebug<CollectionOrIter> {
    fn from(collection: CollectionOrIter) -> Self {
        Self(collection)
    }
}

impl<CollectionOrIter> IntoIterator for SummaryDebug<CollectionOrIter>
where
    CollectionOrIter: IntoIterator,
{
    type Item = <CollectionOrIter as IntoIterator>::Item;
    type IntoIter = <CollectionOrIter as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
