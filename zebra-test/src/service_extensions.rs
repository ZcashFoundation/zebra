//! Extension traits for [`Service`] types to help with testing.

use std::task::Poll;

use futures::future::{BoxFuture, FutureExt};
use tower::{Service, ServiceExt};

/// An extension trait to check if a [`Service`] is immediately ready to be called.
pub trait IsReady<Request>: Service<Request> {
    /// Poll the [`Service`] once, and return true if it is immediately ready to be called.
    #[allow(clippy::wrong_self_convention)]
    fn is_ready(&mut self) -> BoxFuture<'_, bool>;

    /// Poll the [`Service`] once, and return true if it is pending.
    #[allow(clippy::wrong_self_convention)]
    fn is_pending(&mut self) -> BoxFuture<'_, bool>;

    /// Poll the [`Service`] once, and return true if it has failed.
    #[allow(clippy::wrong_self_convention)]
    fn is_failed(&mut self) -> BoxFuture<'_, bool>;
}

impl<S, Request> IsReady<Request> for S
where
    S: Service<Request> + Send,
    Request: 'static,
{
    fn is_ready(&mut self) -> BoxFuture<'_, bool> {
        async move {
            let ready_result = futures::poll!(self.ready());
            matches!(ready_result, Poll::Ready(Ok(_)))
        }
        .boxed()
    }

    fn is_pending(&mut self) -> BoxFuture<'_, bool> {
        async move {
            let ready_result = futures::poll!(self.ready());
            ready_result.is_pending()
        }
        .boxed()
    }

    fn is_failed(&mut self) -> BoxFuture<'_, bool> {
        async move {
            let ready_result = futures::poll!(self.ready());
            matches!(ready_result, Poll::Ready(Err(_)))
        }
        .boxed()
    }
}
