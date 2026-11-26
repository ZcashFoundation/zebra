//! Tests for finalized database blocks and transactions.

#![allow(clippy::unwrap_in_result)]

mod snapshot;
mod vectors;

thread_local! {
    /// Commits a block between a missing-header read and the height-membership check.
    pub(super) static AFTER_MISSING_HEADER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
