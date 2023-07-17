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

    /// Check if `self` contains a panic payload or an unexpected termination, then panic.
    /// Otherwise, return the non-panic part of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload or an unexpected termination.
    #[track_caller]
    fn check_for_panics(self) -> Self::Output;
}

/// A trait that waits for a task to finish, then handles panics and cancellations.
pub trait WaitForPanics {
    /// The underlying task output, after removing panics and unwrapping termination results.
    type Output;

    /// Waits for `self` to finish, then check if its output is:
    /// - a panic payload: resume that panic,
    /// - an unexpected termination: panic with that error,
    /// - an expected termination: hang waiting for shutdown.
    ///
    /// Otherwise, returns the task return value of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload or an unexpected termination.
    ///
    /// # Hangs
    ///
    /// If `self` contains an expected termination, and we're shutting down anyway.
    #[track_caller]
    fn wait_for_panics(self) -> Self::Output;
}
