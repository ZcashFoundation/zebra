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

    /// Panics if the thread panicked.
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

impl<T> WaitForPanics for Arc<JoinHandle<T>> {
    type Output = Option<T>;

    /// If this is the final `Arc`, waits for the thread to finish, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If this is not the final `Arc`, drops the handle and immediately returns `None`.
    #[track_caller]
    fn wait_for_panics(self) -> Self::Output {
        // If we are the last Arc with a reference to this handle,
        // we can wait for it and propagate any panics.
        //
        // We use into_inner() because it guarantees that exactly one of the tasks gets the
        // JoinHandle. try_unwrap() lets us keep the JoinHandle, but it can also miss panics.
        //
        // This is more readable as an expanded statement.
        #[allow(clippy::manual_map)]
        if let Some(handle) = Arc::into_inner(self) {
            Some(handle.wait_for_panics())
        } else {
            None
        }
    }
}

impl<T> CheckForPanics for &mut Option<Arc<JoinHandle<T>>> {
    type Output = Option<T>;

    /// If this is the final `Arc`, checks if the thread has finished, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If the thread has not finished, or this is not the final `Arc`, returns `None`.
    #[track_caller]
    fn check_for_panics(self) -> Self::Output {
        let handle = self.take()?;

        if handle.is_finished() {
            // This is the same as calling `self.wait_for_panics()`, but we can't do that,
            // because we've taken `self`.
            #[allow(clippy::manual_map)]
            return handle.wait_for_panics();
        }

        *self = Some(handle);

        None
    }
}

impl<T> WaitForPanics for &mut Option<Arc<JoinHandle<T>>> {
    type Output = Option<T>;

    /// If this is the final `Arc`, waits for the thread to finish, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If this is not the final `Arc`, drops the handle and returns `None`.
    #[track_caller]
    fn wait_for_panics(self) -> Self::Output {
        // This is more readable as an expanded statement.
        #[allow(clippy::manual_map)]
        if let Some(output) = self.take()?.wait_for_panics() {
            Some(output)
        } else {
            // Some other task has a reference, so we should give up ours to let them use it.
            None
        }
    }
}
