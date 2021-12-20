//! Extension traits for [`Service`] types to help with testing.

use futures::future::{BoxFuture, FutureExt};
use tower::{Service, ServiceExt};

use now_or_later::NowOrLater;

/// An extension trait to check if a [`Service`] is immediately ready to be called.
pub trait IsReady<Request>: Service<Request> {
    /// Check if the [`Service`] is immediately ready to be called.
    ///
    /// Takes ownership of the service, so this method can only be called once.
    /// Some services will
    /// [reserve capacity until the next call](https://docs.rs/tower/0.4.11/tower/buffer/struct.Buffer.html#a-note-on-choosing-a-bound).
    /// So calling this method multiple times risks capacity exhaustion in the service,
    /// or in the services or channels that it checks for readiness.
    ///
    /// TODO: return the service if it is ready (#3261)
    ///       put a `must_use` on each method
    ///       fix the self convention: https://rust-lang.github.io/rust-clippy/master/#wrong_self_convention
    #[allow(clippy::wrong_self_convention)]
    fn is_ready(self) -> BoxFuture<'static, bool>;

    /// Check if the [`Service`] is not ready to be called.
    ///
    /// Takes ownership of the service, so this method can only be called once.
    /// See [IsReady::is_ready] for details.
    #[allow(clippy::wrong_self_convention)]
    fn is_pending(self) -> BoxFuture<'static, bool>;

    /// Check if the [`Service`] is not immediately ready because it returns an error.
    ///
    /// Takes ownership of the service, so this method can only be called once.
    /// See [IsReady::is_ready] for details.
    #[allow(clippy::wrong_self_convention)]
    fn is_failed(self) -> BoxFuture<'static, bool>;
}

impl<S, Request> IsReady<Request> for S
where
    S: Service<Request> + Send + 'static,
    Request: 'static,
{
    fn is_ready(self) -> BoxFuture<'static, bool> {
        NowOrLater(self.ready_oneshot())
            .map(|ready_result| matches!(ready_result, Some(Ok(_))))
            .boxed()
    }

    fn is_pending(self) -> BoxFuture<'static, bool> {
        NowOrLater(self.ready_oneshot())
            .map(|ready_result| ready_result.is_none())
            .boxed()
    }

    fn is_failed(self) -> BoxFuture<'static, bool> {
        NowOrLater(self.ready_oneshot())
            .map(|ready_result| matches!(ready_result, Some(Err(_))))
            .boxed()
    }
}
