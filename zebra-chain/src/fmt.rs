//! Format wrappers for Zebra

use std::fmt;

pub struct DisplayToDebug<T>(pub T);

impl<T> fmt::Debug for DisplayToDebug<T>
where
    T: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

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
