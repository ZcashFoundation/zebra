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
pub trait State: ZebraService<Request, Response> {}

/// Blanket impl for any matching service.
impl<T> State for T where T: ZebraService<Request, Response> {}

/// Trait alias for services handling read-only state requests.
pub trait ReadState: ZebraService<ReadRequest, ReadResponse> {}

/// Blanket impl for any matching service.
impl<T> ReadState for T where T: ZebraService<ReadRequest, ReadResponse> {}

pub trait ZebraService<Request, Response, Err = BoxError>:
    Service<Request, Response = Response, Error = Err, Future: Send + 'static>
    + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T, Request, Response, Err> ZebraService<Request, Response, Err> for T where
    T: Service<Request, Response = Response, Error = Err, Future: Send + 'static>
        + Clone
        + Send
        + Sync
        + 'static
{
}
