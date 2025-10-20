//! Trait aliases for Tower services used in state handling.
//!
//! These traits abstract over Tower `Service` implementations that
//! handle state-modifying requests (`StateServiceTrait`) and
//! read-only state requests (`ReadStateServiceTrait`).
//!
//! They enforce bounds such as `Clone`, `Send`, `Sync` and require the
//! associated futures to be `Send`, making them suitable for
//! multi-threaded async contexts.

use crate::{BoxError, ReadRequest, ReadResponse, Request, Response};
use tower::Service;

/// Trait alias for services handling state-modifying requests.
pub trait StateServiceTrait:
    Service<Request, Response = Response, Error = BoxError, Future: Send + 'static>
    + Clone
    + Send
    + Sync
    + 'static
{
}

/// Blanket impl for any matching service.
impl<T> StateServiceTrait for T where
    T: Service<Request, Response = Response, Error = BoxError, Future: Send + 'static>
        + Clone
        + Send
        + Sync
        + 'static
{
}

/// Trait alias for services handling read-only state requests.
pub trait ReadStateServiceTrait:
    Service<ReadRequest, Response = ReadResponse, Error = BoxError, Future: Send + 'static>
    + Clone
    + Send
    + Sync
    + 'static
{
}

/// Blanket impl for any matching service.
impl<T> ReadStateServiceTrait for T where
    T: Service<ReadRequest, Response = ReadResponse, Error = BoxError, Future: Send + 'static>
        + Clone
        + Send
        + Sync
        + 'static
{
}
