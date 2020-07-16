use std::task::{Context, Poll};

use tower::Service;

use super::future::ResponseFuture;
use crate::BoxedError;

/// Provides fallback processing on a second service if the first service returned an error.
#[derive(Debug)]
pub struct Fallback<S1, S2>
where
    S2: Clone,
{
    svc1: S1,
    svc2: S2,
}

impl<S1: Clone, S2: Clone> Clone for Fallback<S1, S2> {
    fn clone(&self) -> Self {
        Self {
            svc1: self.svc1.clone(),
            svc2: self.svc2.clone(),
        }
    }
}

impl<S1, S2: Clone> Fallback<S1, S2> {
    /// Creates a new `Fallback` wrapping a pair of services.
    ///
    /// Requests are processed on `svc1`, and retried on `svc2` if `svc1` errored.
    pub fn new(svc1: S1, svc2: S2) -> Self {
        Self { svc1, svc2 }
    }
}

impl<S1, S2, Request> Service<Request> for Fallback<S1, S2>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    S1::Error: Into<BoxedError>,
    S2::Error: Into<BoxedError>,
    S2: Clone,
    Request: Clone,
{
    type Response = <S1 as Service<Request>>::Response;
    type Error = BoxedError;
    type Future = ResponseFuture<S1, S2, Request>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.svc1.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let request2 = request.clone();
        ResponseFuture::new(self.svc1.call(request), request2, self.svc2.clone())
    }
}
