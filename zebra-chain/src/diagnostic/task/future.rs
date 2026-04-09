//! Diagnostic types and functions for Zebra async future tasks:
//! - task handles
//! - errors and panics

use std::{future, panic};

use futures::future::{BoxFuture, FutureExt};
use tokio::task::{JoinError, JoinHandle};

use crate::shutdown::is_shutting_down;

use super::{CheckForPanics, WaitForPanics};

/// This is the return type of the [`JoinHandle`] future.
impl<T> CheckForPanics for Result<T, JoinError> {
    /// The [`JoinHandle`]'s task output, after resuming any panics,
    /// and ignoring task cancellations on shutdown.
    type Output = Result<T, JoinError>;

    /// Returns the task result if the task finished normally.
    /// Otherwise, resumes any panics, and ignores any expected errors.
    /// Handles unexpected errors based on `panic_on_unexpected_termination`.
    ///
    /// If the task finished normally, returns `Some(T)`.
    /// If the task was cancelled, returns `None`.
    #[track_caller]
    fn check_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        match self {
            Ok(task_output) => Ok(task_output),
            Err(join_error) => {
                Err(join_error.check_for_panics_with(panic_on_unexpected_termination))
            }
        }
    }
}

impl CheckForPanics for JoinError {
    /// The [`JoinError`] after resuming any panics, and logging any unexpected task cancellations.
    type Output = JoinError;

    /// Resume any panics and panic on unexpected task cancellations.
    /// Always returns [`JoinError::Cancelled`](JoinError::is_cancelled).
    #[track_caller]
    fn check_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        match self.try_into_panic() {
            Ok(panic_payload) => panic::resume_unwind(panic_payload),

            // We could ignore this error, but then we'd have to change the return type.
            Err(task_cancelled) => {
                if !panic_on_unexpected_termination {
                    debug!(?task_cancelled, "ignoring expected task termination");

                    task_cancelled
                } else if is_shutting_down() {
                    debug!(
                        ?task_cancelled,
                        "ignoring task termination because Zebra is shutting down"
                    );

                    task_cancelled
                } else {
                    panic!("task unexpectedly exited with: {task_cancelled:?}")
                }
            }
        }
    }
}

impl<T> WaitForPanics for JoinHandle<T>
where
    T: Send + 'static,
{
    type Output = BoxFuture<'static, T>;

    /// Returns a future which waits for `self` to finish, then checks if its output is:
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
    /// If `self` contains a panic payload, or [`JoinHandle::abort()`] has been called on `self`.
    ///
    /// # Hangs
    ///
    /// If `self` contains an expected termination, and we're shutting down anyway.
    /// If we're ignoring terminations because `panic_on_unexpected_termination` is `false`.
    /// Futures hang by returning `Pending` and not setting a waker, so this uses minimal resources.
    #[track_caller]
    fn wait_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        async move {
            match self
                .await
                .check_for_panics_with(panic_on_unexpected_termination)
            {
                Ok(task_output) => task_output,
                Err(_expected_cancel_error) => future::pending().await,
            }
        }
        .boxed()
    }
}
