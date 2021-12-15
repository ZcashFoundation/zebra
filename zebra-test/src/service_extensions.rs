//! Extension traits for [`Service`] types to help with testing.

use futures::future::{BoxFuture, FutureExt};
use tower::{Service, ServiceExt};

use now_or_later::NowOrLater;

/// An extension trait to check if a [`Service`] is immediately ready to be called.
pub trait IsReady<Request>: Service<Request> {
    /// Check if the [`Service`] is immediately ready to be called.
    fn is_ready(&mut self) -> BoxFuture<bool>;
}

impl<S, Request> IsReady<Request> for S
where
    S: Service<Request> + Send,
    Request: 'static,
{
    fn is_ready(&mut self) -> BoxFuture<bool> {
        NowOrLater(self.ready())
            .map(|ready_result| matches!(ready_result, Some(Ok(_))))
            .boxed()
    }
}
