//! Diagnostic types and functions for Zebra tasks:
//! - OS thread handling
//! - async future task handling
//! - errors and panics

#[cfg(feature = "async-error")]
pub mod future;

pub mod thread;

/// A trait that checks a task's return value for panics.
pub trait CheckForPanics: Sized {
    /// The output type, after removing panics from `Self`.
    type Output;

    /// Check if `self` contains a panic payload or an unexpected termination, then panic.
    /// Otherwise, return the non-panic part of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload or an unexpected termination.
    #[track_caller]
    fn panic_if_task_has_finished(self) -> Self::Output {
        self.check_for_panics_with(true)
    }

    /// Check if `self` contains a panic payload, then panic.
    /// Otherwise, return the non-panic part of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload.
    #[track_caller]
    fn panic_if_task_has_panicked(self) -> Self::Output {
        self.check_for_panics_with(false)
    }

    /// Check if `self` contains a panic payload, then panic. Also panics if
    /// `panic_on_unexpected_termination` is true, and `self` is an unexpected termination.
    /// Otherwise, return the non-panic part of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload, or if we're panicking on unexpected terminations.
    #[track_caller]
    fn check_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output;
}

/// A trait that waits for a task to finish, then handles panics and cancellations.
pub trait WaitForPanics: Sized {
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
    fn wait_and_panic_on_unexpected_termination(self) -> Self::Output {
        self.wait_for_panics_with(true)
    }

    /// Waits for `self` to finish, then check if its output is:
    /// - a panic payload: resume that panic,
    /// - a task termination: hang waiting for shutdown.
    ///
    /// Otherwise, returns the task return value of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload.
    ///
    /// # Hangs
    ///
    /// If `self` contains a task termination.
    #[track_caller]
    fn wait_for_panics(self) -> Self::Output {
        self.wait_for_panics_with(false)
    }

    /// Waits for `self` to finish, then check if its output is:
    /// - a panic payload: resume that panic,
    /// - an unexpected termination:
    ///   - if `panic_on_unexpected_termination` is true, panic with that error,
    ///   - otherwise, hang waiting for shutdown,
    /// - an expected termination: hang waiting for shutdown.
    ///
    /// Otherwise, returns the task return value of `self`.
    ///
    /// # Panics
    ///
    /// If `self` contains a panic payload, or if we're panicking on unexpected terminations.
    ///
    /// # Hangs
    ///
    /// If `self` contains an expected or ignored termination, and we're shutting down anyway.
    #[track_caller]
    fn wait_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output;
}
