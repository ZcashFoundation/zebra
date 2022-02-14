//! The status of a response to an inventory request.

use std::fmt;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use ResponseStatus::*;

/// A generic peer inventory response status.
///
/// `Available` is used for inventory that is present in the response,
/// and `Missing` is used for inventory that is missing from the response.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum ResponseStatus<A, M> {
    /// An available inventory item.
    Available(A),

    /// A missing inventory item.
    Missing(M),
}

impl<A, M> fmt::Display for ResponseStatus<A, M> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.command())
    }
}

impl<A, M> ResponseStatus<A, M> {
    /// Returns the response status type as a string.
    pub fn command(&self) -> &'static str {
        match self {
            ResponseStatus::Available(_) => "Available",
            ResponseStatus::Missing(_) => "Missing",
        }
    }

    /// Returns true if the inventory item was available.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        matches!(self, Available(_))
    }

    /// Returns true if the inventory item was missing.
    #[allow(dead_code)]
    pub fn is_missing(&self) -> bool {
        matches!(self, Missing(_))
    }

    /// Maps a `ResponseStatus<A, M>` to `ResponseStatus<B, M>` by applying a function to a
    /// contained [`Available`] value, leaving the [`Missing`] value untouched.
    #[allow(dead_code)]
    pub fn map_available<B, F: FnOnce(A) -> B>(self, f: F) -> ResponseStatus<B, M> {
        // Based on Result::map from https://doc.rust-lang.org/src/core/result.rs.html#765
        match self {
            Available(a) => Available(f(a)),
            Missing(m) => Missing(m),
        }
    }

    /// Maps a `ResponseStatus<A, M>` to `ResponseStatus<A, N>` by applying a function to a
    /// contained [`Missing`] value, leaving the [`Available`] value untouched.
    #[allow(dead_code)]
    pub fn map_missing<N, F: FnOnce(M) -> N>(self, f: F) -> ResponseStatus<A, N> {
        // Based on Result::map_err from https://doc.rust-lang.org/src/core/result.rs.html#850
        match self {
            Available(a) => Available(a),
            Missing(m) => Missing(f(m)),
        }
    }

    /// Converts from `&ResponseStatus<A, M>` to `ResponseStatus<&A, &M>`.
    pub fn as_ref(&self) -> ResponseStatus<&A, &M> {
        match self {
            Available(item) => Available(item),
            Missing(item) => Missing(item),
        }
    }
}

impl<A: Clone, M: Clone> ResponseStatus<A, M> {
    /// Get the available inventory item, if present.
    pub fn available(&self) -> Option<A> {
        if let Available(item) = self {
            Some(item.clone())
        } else {
            None
        }
    }

    /// Get the missing inventory item, if present.
    #[allow(dead_code)]
    pub fn missing(&self) -> Option<M> {
        if let Missing(item) = self {
            Some(item.clone())
        } else {
            None
        }
    }
}
