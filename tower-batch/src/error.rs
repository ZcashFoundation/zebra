//! Error types for the `Batch` middleware.

use std::fmt;

/// An error produced when the batch worker closes unexpectedly.
pub struct Closed {
    _p: (),
}

// ===== impl Closed =====

impl Closed {
    pub(crate) fn new() -> Self {
        Closed { _p: () }
    }
}

impl fmt::Debug for Closed {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_tuple("Closed").finish()
    }
}

impl fmt::Display for Closed {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str("batch worker closed unexpectedly")
    }
}

impl std::error::Error for Closed {}
