//! Diagnostic types and functions for Zebra OS thread tasks:
//! - task handles
//! - errors and panics

use std::{
    panic,
    sync::Arc,
    thread::{self, JoinHandle},
};

use super::{CheckForPanics, WaitForPanics};

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

impl<T> WaitForPanics for JoinHandle<T> {
    type Output = T;

    /// Waits for the thread to finish, then panics if the thread panicked.
    #[track_caller]
    fn wait_for_panics(self) -> Self::Output {
        self.join().check_for_panics()
    }
}
