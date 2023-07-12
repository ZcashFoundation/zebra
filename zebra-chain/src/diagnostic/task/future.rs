//! Diagnostic types and functions for Zebra async future tasks:
//! - task handles
//! - errors and panics

use std::panic;

use tokio::task;

use crate::shutdown::is_shutting_down;

use super::CheckForPanics;

// Only for doc links.
#[allow(unused_imports)]
use tokio::task::JoinHandle;

/// This is the return type of the [`JoinHandle`] future.
impl<T> CheckForPanics for Result<T, task::JoinError> {
    /// The [`JoinHandle`]'s task output, after resuming any panics,
    /// and ignoring task cancellations on shutdown.
    type Output = Result<T, task::JoinError>;

    /// Returns the task result if the task finished normally.
    /// Otherwise, resumes any panics, logs unexpected errors, and ignores any expected errors.
    ///
    /// If the task finished normally, returns `Some(T)`.
    /// If the task was cancelled, returns `None`.
    fn check_for_panics(self) -> Self::Output {
        match self {
            Ok(task_output) => Ok(task_output),
            Err(join_error) => Err(join_error.check_for_panics()),
        }
    }
}

impl CheckForPanics for task::JoinError {
    /// The [`JoinError`] after resuming any panics, and logging any unexpected task cancellations.
    type Output = task::JoinError;

    /// Resume any panics and panic on unexpected task cancellations.
    /// Always returns [`JoinError::Cancelled`].
    fn check_for_panics(self) -> Self::Output {
        match self.try_into_panic() {
            Ok(panic_payload) => panic::resume_unwind(panic_payload),

            // We could ignore this error, but then we'd have to change the return type.
            Err(task_cancelled) if is_shutting_down() => {
                debug!(
                    ?task_cancelled,
                    "ignoring cancelled task because Zebra is shutting down"
                );

                task_cancelled
            }

            Err(task_cancelled) => {
                panic!("task cancelled during normal Zebra operation: {task_cancelled:?}");
            }
        }
    }
}
