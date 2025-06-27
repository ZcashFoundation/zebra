//! Diagnostic types and functions for Zebra OS thread tasks:
//! - task handles
//! - errors and panics

use std::{
    panic,
    sync::Arc,
    thread::{self, JoinHandle},
};

use crate::shutdown::is_shutting_down;

use super::{CheckForPanics, WaitForPanics};

impl<T> CheckForPanics for thread::Result<T>
where
    T: std::fmt::Debug,
{
    type Output = T;

    /// # Panics
    ///
    /// - if the thread panicked.
    /// - if the thread is cancelled, `panic_on_unexpected_termination` is true, and
    ///   Zebra is not shutting down.
    ///
    /// Threads can't be cancelled except by using a panic, so there are no thread errors here.
    /// `panic_on_unexpected_termination` is
    #[track_caller]
    fn check_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        match self {
            // The value returned by the thread when it finished.
            Ok(thread_output) => {
                if !panic_on_unexpected_termination {
                    debug!(?thread_output, "ignoring expected thread exit");

                    thread_output
                } else if is_shutting_down() {
                    debug!(
                        ?thread_output,
                        "ignoring thread exit because Zebra is shutting down"
                    );

                    thread_output
                } else {
                    panic!("thread unexpectedly exited with: {thread_output:?}")
                }
            }

            // A thread error is always a panic.
            Err(panic_payload) => panic::resume_unwind(panic_payload),
        }
    }
}

impl<T> WaitForPanics for JoinHandle<T>
where
    T: std::fmt::Debug,
{
    type Output = T;

    /// Waits for the thread to finish, then panics if the thread panicked.
    #[track_caller]
    fn wait_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        self.join()
            .check_for_panics_with(panic_on_unexpected_termination)
    }
}

impl<T> WaitForPanics for Arc<JoinHandle<T>>
where
    T: std::fmt::Debug,
{
    type Output = Option<T>;

    /// If this is the final `Arc`, waits for the thread to finish, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If this is not the final `Arc`, drops the handle and immediately returns `None`.
    #[track_caller]
    fn wait_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        // If we are the last Arc with a reference to this handle,
        // we can wait for it and propagate any panics.
        //
        // We use into_inner() because it guarantees that exactly one of the tasks gets the
        // JoinHandle. try_unwrap() lets us keep the JoinHandle, but it can also miss panics.
        //
        // This is more readable as an expanded statement.
        #[allow(clippy::manual_map)]
        if let Some(handle) = Arc::into_inner(self) {
            Some(handle.wait_for_panics_with(panic_on_unexpected_termination))
        } else {
            None
        }
    }
}

impl<T> CheckForPanics for &mut Option<Arc<JoinHandle<T>>>
where
    T: std::fmt::Debug,
{
    type Output = Option<T>;

    /// If this is the final `Arc`, checks if the thread has finished, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If the thread has not finished, or this is not the final `Arc`, returns `None`.
    #[track_caller]
    fn check_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        let handle = self.take()?;

        if handle.is_finished() {
            // This is the same as calling `self.wait_for_panics()`, but we can't do that,
            // because we've taken `self`.
            #[allow(clippy::manual_map)]
            return handle.wait_for_panics_with(panic_on_unexpected_termination);
        }

        *self = Some(handle);

        None
    }
}

impl<T> WaitForPanics for &mut Option<Arc<JoinHandle<T>>>
where
    T: std::fmt::Debug,
{
    type Output = Option<T>;

    /// If this is the final `Arc`, waits for the thread to finish, then panics if the thread
    /// panicked. Otherwise, returns the thread's return value.
    ///
    /// If this is not the final `Arc`, drops the handle and returns `None`.
    #[track_caller]
    fn wait_for_panics_with(self, panic_on_unexpected_termination: bool) -> Self::Output {
        // This is more readable as an expanded statement.
        #[allow(clippy::manual_map)]
        if let Some(output) = self
            .take()?
            .wait_for_panics_with(panic_on_unexpected_termination)
        {
            Some(output)
        } else {
            // Some other task has a reference, so we should give up ours to let them use it.
            None
        }
    }
}
