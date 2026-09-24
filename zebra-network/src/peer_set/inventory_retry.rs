//! Bounded, cancellation-safe waiting for busy block inventory peers.

use std::{
    collections::HashSet,
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};

use tokio::{
    task::yield_now,
    time::{sleep, sleep_until, Instant},
};
use tower::{Service, ServiceExt};

use crate::{constants, BoxError, PeerError, PeerSocketAddr, Request, Response, SharedPeerError};

#[cfg(test)]
mod tests;

/// A request and the peers that explicitly rejected it during this logical request.
#[derive(Debug)]
pub(super) struct InventoryRequest {
    pub request: Request,
    pub attempted: HashSet<PeerSocketAddr>,
}

impl From<Request> for InventoryRequest {
    fn from(request: Request) -> Self {
        Self {
            request,
            attempted: HashSet::new(),
        }
    }
}

/// Retry context after a busy routing attempt or an explicit peer rejection.
#[derive(Debug, thiserror::Error)]
#[error("eligible block inventory peers are busy")]
pub(super) struct InventoryBusy {
    pub attempted: HashSet<PeerSocketAddr>,
    pub source: Option<BoxError>,
}

/// Retry single-block inventory requests without consuming caller retry budgets.
///
/// Initial queueing, retry readiness, and retry waits share one absolute request deadline.
/// Dropping the returned future cancels its request or timer; no retry task is spawned.
pub(super) fn retry_busy_inventory<S>(service: S) -> InventoryRetry<S> {
    InventoryRetry(service)
}

/// Preserve the inner buffer's admission backpressure while retrying accepted requests.
#[derive(Clone)]
pub(super) struct InventoryRetry<S>(S);

impl<S> Service<Request> for InventoryRetry<S>
where
    S: Service<InventoryRequest, Response = Response, Error = BoxError> + Clone + Send + 'static,
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
            .then(|| (request.clone(), Instant::now() + constants::REQUEST_TIMEOUT));
        // Use the service whose readiness we reserved, not a fresh clone.
        let first = self.0.call(request.into());
        let Some((request, deadline)) = retry_request else {
            return first.boxed();
        };
        let mut service = self.0.clone();
        async move {
            let mut last_missing = None;
            let result = tokio::select! {
                // An expired request must not be dispatched again, even if its response
                // future was left unpolled until after the deadline.
                biased;
                _ = sleep_until(deadline) => None,
                result = async {
                    let mut result = first.await;
                    loop {
                        let error = match result {
                            Ok(response) => return Ok(response),
                            Err(error) => error,
                        };
                        let InventoryBusy { attempted, source } = match error.downcast::<InventoryBusy>() {
                            Ok(busy) => *busy,
                            Err(error) => return Err(error),
                        };
                        if let Some(error) = source {
                            last_missing = Some(error);
                            yield_now().await;
                        } else {
                            sleep(Duration::from_secs(1)).await;
                        }
                        result = service.ready().await?.call(InventoryRequest {
                            request: request.clone(),
                            attempted,
                        }).await;
                    }
                } => Some(result),
            };
            result.unwrap_or_else(|| {
                Err(last_missing.unwrap_or_else(|| {
                    SharedPeerError::from(PeerError::ConnectionReceiveTimeout).into()
                }))
            })
        }
        .boxed()
    }
}
