use std::task::{Context, Poll};

use tower::Service;

use super::future::ResponseFuture;
use crate::BoxedError;

/// Decides if a [`Fallback`] service should call its fallback service.
pub trait FallbackPolicy<Response> {
    /// Returns `true` if the fallback service should handle this request.
    fn should_fallback(&self, result: &Result<Response, BoxedError>) -> bool;
}

impl<Response, F> FallbackPolicy<Response> for F
where
    F: Fn(&Result<Response, BoxedError>) -> bool,
{
    fn should_fallback(&self, result: &Result<Response, BoxedError>) -> bool {
        self(result)
    }
}

/// The default fallback policy.
///
/// Falls back whenever the first service returns an error.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OnError;

impl<Response> FallbackPolicy<Response> for OnError {
    fn should_fallback(&self, result: &Result<Response, BoxedError>) -> bool {
        result.is_err()
    }
}

/// Provides fallback processing on a second service if its fallback policy selects it.
#[derive(Debug)]
pub struct Fallback<S1, S2, F = OnError>
where
    S2: Clone,
{
    svc1: S1,
    svc2: S2,
    policy: F,
}

impl<S1: Clone, S2: Clone, F: Clone> Clone for Fallback<S1, S2, F> {
    fn clone(&self) -> Self {
        Self {
            svc1: self.svc1.clone(),
            svc2: self.svc2.clone(),
            policy: self.policy.clone(),
        }
    }
}

impl<S1, S2: Clone> Fallback<S1, S2, OnError> {
    /// Creates a new `Fallback` wrapping a pair of services.
    ///
    /// Requests are processed on `svc1`, and retried on `svc2` if `svc1` errored.
    pub fn new(svc1: S1, svc2: S2) -> Self {
        Self {
            svc1,
            svc2,
            policy: OnError,
        }
    }
}

impl<S1, S2: Clone, F: Clone> Fallback<S1, S2, F> {
    /// Creates a new `Fallback` wrapping a pair of services with a custom fallback policy.
    ///
    /// Requests are processed on `svc1`, and retried on `svc2` if `policy` returns `true`
    /// for `svc1`'s result.
    pub fn new_with_policy(svc1: S1, svc2: S2, policy: F) -> Self {
        Self { svc1, svc2, policy }
    }
}

impl<S1, S2, F, Request> Service<Request> for Fallback<S1, S2, F>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    F: FallbackPolicy<<S1 as Service<Request>>::Response> + Clone,
    S1::Error: Into<BoxedError>,
    S2::Error: Into<BoxedError>,
    S2: Clone,
    Request: Clone,
{
    type Response = <S1 as Service<Request>>::Response;
    type Error = BoxedError;
    type Future = ResponseFuture<S1, S2, Request, F>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.svc1.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let request2 = request.clone();
        ResponseFuture::new(
            self.svc1.call(request),
            request2,
            self.svc2.clone(),
            self.policy.clone(),
        )
    }
}
