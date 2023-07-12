//! Diagnostic types and functions for Zebra OS thread tasks:
//! - task handles
//! - errors and panics

use std::{panic, thread};

use super::CheckForPanics;

impl<T> CheckForPanics for thread::Result<T> {
    type Output = T;

    fn check_for_panics(self) -> Self::Output {
        match self {
            // The value returned by the thread when it finished.
            Ok(thread_output) => thread_output,

            // A thread error is always a panic.
            Err(panic_payload) => panic::resume_unwind(panic_payload),
        }
    }
}
