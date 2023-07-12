//! Diagnostic types and functions for Zebra tasks:
//! - OS thread handling
//! - async future task handling
//! - errors and panics

#[cfg(feature = "async-error")]
pub mod future;

pub mod thread;

/// A trait that checks a task's return value for panics.
pub trait CheckForPanics {
    /// The output type, after removing panics from `Self`.
    type Output;

    /// Check if `self` contains a panic payload, and resume that panic.
    /// Otherwise, return the non-panic part of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload.
    fn check_for_panics(self) -> Self::Output;
}
