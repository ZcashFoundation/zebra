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

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryDebug<T>(pub T);

impl<T> fmt::Debug for SummaryDebug<Vec<T>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, len={}", std::any::type_name::<T>(), self.0.len())
    }
}

impl<T> fmt::Debug for SummaryDebug<&Vec<T>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, len={}", std::any::type_name::<T>(), self.0.len())
    }
}

impl<T> ops::Deref for SummaryDebug<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> From<T> for SummaryDebug<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

impl<T> IntoIterator for SummaryDebug<Vec<T>> {
    type Item = T;

    type IntoIter = <Vec<T> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
