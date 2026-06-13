//! Error types for the `FairBuffer` middleware.

use std::{fmt, sync::Arc};

use crate::BoxError;

/// An error produced by a `Service` wrapped by a `FairBuffer`.
#[derive(Debug)]
pub struct ServiceError {
    inner: Arc<BoxError>,
}

/// An error produced when the fair buffer's worker closes unexpectedly.
pub struct Closed {
    _p: (),
}

/// An error produced when the fair buffer is full and a queued request is
/// shed to make room.
///
/// The request that is shed is the queued request whose caller had the
/// highest recent request count when it was enqueued. Internal requests are
/// never shed.
///
/// This error deliberately doesn't contain the caller key, so logging it
/// can't leak caller identities like peer IP addresses.
pub struct Shed {
    _p: (),
}

// ===== impl ServiceError =====

impl ServiceError {
    pub(crate) fn new(inner: BoxError) -> ServiceError {
        let inner = Arc::new(inner);
        ServiceError { inner }
    }

    // Private to avoid exposing `Clone` trait as part of the public API
    pub(crate) fn clone(&self) -> ServiceError {
        ServiceError {
            inner: self.inner.clone(),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "fair buffered service failed: {}", self.inner)
    }
}

impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&**self.inner)
    }
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
        fmt.write_str("fair buffer's worker closed unexpectedly")
    }
}

impl std::error::Error for Closed {}

// ===== impl Shed =====

impl Shed {
    pub(crate) fn new() -> Self {
        Shed { _p: () }
    }
}

impl fmt::Debug for Shed {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_tuple("Shed").finish()
    }
}

impl fmt::Display for Shed {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str("fair buffer overloaded: request shed")
    }
}

impl std::error::Error for Shed {}
