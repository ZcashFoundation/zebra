//! Bounded, cancellation-safe waiting for busy block inventory peers.

use std::{
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};

use tokio::time::{sleep, timeout};
use tower::{util::BoxService, Service, ServiceExt};

use crate::{constants, BoxError, PeerError, Request, Response, SharedPeerError};

#[cfg(test)]
mod tests;

/// Eligible peers are busy, rather than known to be missing the requested block.
#[derive(Debug, thiserror::Error)]
#[error("eligible block inventory peers are busy")]
pub(super) struct InventoryBusy;

/// Retry only busy single-block requests, without consuming caller retry budgets.
///
/// Requests, retry readiness, and one-second retry waits share one request timeout.
/// Dropping the returned future cancels its request or timer; no retry task is spawned.
pub(super) fn retry_busy_inventory<S>(service: S) -> BoxService<Request, Response, BoxError>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    BoxService::new(InventoryRetry(service))
}

/// Preserve the inner buffer's admission backpressure while retrying accepted requests.
struct InventoryRetry<S>(S);

impl<S> Service<Request> for InventoryRetry<S>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let retry_request = matches!(&request, Request::BlocksByHash(hashes) if hashes.len() == 1)
            .then(|| request.clone());
        // Use the service whose readiness we reserved, not a fresh clone.
        let first = self.0.call(request);
        let Some(request) = retry_request else {
            return first.boxed();
        };
        let mut service = self.0.clone();
        timeout(constants::REQUEST_TIMEOUT, async move {
            let mut result = first.await;
            loop {
                match result {
                    Err(error) if error.is::<InventoryBusy>() => {
                        sleep(Duration::from_secs(1)).await;
                        result = service.ready().await?.call(request.clone()).await;
                    }
                    result => return result,
                }
            }
        })
        .map(|result| {
            result.unwrap_or_else(|_| {
                Err(SharedPeerError::from(PeerError::ConnectionReceiveTimeout).into())
            })
        })
        .boxed()
    }
}
