//! Diagnostic types and functions for Zebra OS thread tasks:
//! - task handles
//! - errors and panics

use std::{panic, thread};

use super::{CheckForPanics, WaitForTermination};

impl<T> CheckForPanics for thread::Result<T> {
    type Output = T;

    /// Waits for the thread to finish, then panics if the thread panicked.
    ///
    /// Threads can't be cancelled except by using a panic, so there are no thread errors here.
    #[track_caller]
    fn check_for_panics(self) -> Self::Output {
        match self {
            // The value returned by the thread when it finished.
            Ok(thread_output) => thread_output,

            // A thread error is always a panic.
            Err(panic_payload) => panic::resume_unwind(panic_payload),
        }
    }
}

impl<T> WaitForTermination for thread::JoinHandle<T> {
    type UnwrappedOutput = T;

    type ResultOutput = thread::Result<T>;

    /// Waits for the thread to finish, then panics if the thread panicked.
    #[track_caller]
    fn panic_on_early_termination(self) -> Self::UnwrappedOutput {
        self.join().check_for_panics()
    }

    /// Waits for the thread to finish, then panics if the thread panicked.
    ///
    /// Threads can't be cancelled except by using a panic, so the returned result is always `Ok`.
    #[track_caller]
    fn error_on_early_termination<D>(self, _expected_termination_value: D) -> Self::ResultOutput
    where
        D: Into<Self::ResultOutput> + 'static,
    {
        Ok(self.panic_on_early_termination())
    }
}
