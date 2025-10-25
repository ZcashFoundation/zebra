//! A trait alias for Zebra services based on `tower::Service`.

use crate::BoxError;
use tower::Service;

/// A supertrait for Zebra services.
///
/// It adds common bounds like `Clone`, `Send`, and `Sync`,
/// and uses `BoxError` as the default error type.
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
