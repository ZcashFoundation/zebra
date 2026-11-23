//! Preserve a live hedge arm when its sibling exhausts its retries.
//!
//! Tower still owns latency sampling, warmup, delay, and histogram rotation. Only its arm
//! responses are coordinated: an error cannot win while another arm can still succeed.
//! If every arm fails, preserve an original `NotFound` error in preference to other failures,
//! regardless of completion order, so sync can re-request the missing block without restarting.
//!
//! Readiness errors join this same race without dispatching to the failed service. Downloads
//! therefore reports them as `DownloadFailed`, rather than `NetworkServiceError`.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use futures::{ready, task::AtomicWaker};
use pin_project::pin_project;
use tower::{hedge, BoxError, Service};
use zebra_network as zn;

use super::{is_not_found, SYNC_RESTART_DELAY};

#[cfg(test)]
mod tests;

/// Tower's usual hedge, preferring success over an already-completed sibling error.
#[derive(Debug)]
pub(super) struct SuccessHedge<S>(hedge::Hedge<ArmService<S>, HedgePolicy>);

impl<S> SuccessHedge<S>
where
    S: Service<zn::Request> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    /// Wrap a retrying service without changing its retry policy or Tower's hedge timing.
    pub(super) fn new(service: S) -> Self {
        Self(hedge::Hedge::new(
            ArmService {
                inner: service,
                ready_error: None,
            },
            HedgePolicy,
            20,
            0.95,
            2 * SYNC_RESTART_DELAY,
        ))
    }
}

impl<S> Service<zn::Request> for SuccessHedge<S>
where
    S: Service<zn::Request> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = <hedge::Hedge<ArmService<S>, HedgePolicy> as Service<HedgeRequest>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        self.0.call(HedgeRequest {
            request,
            arm: Arm {
                state: Arc::new(ArmState {
                    flags: AtomicU8::new(1),
                    wakers: [AtomicWaker::new(), AtomicWaker::new()],
                    error: Mutex::new(None),
                }),
                index: 0,
            },
        })
    }
}

#[derive(Debug)]
struct ArmState {
    /// Bits 0 and 1 mark live arms.
    flags: AtomicU8,
    wakers: [AtomicWaker; 2],
    /// The preferred terminal error, consumed once by the outer hedge.
    error: Mutex<Option<BoxError>>,
}

/// One non-cloneable owner, first in the request and then in its response future.
#[derive(Debug)]
struct Arm {
    state: Arc<ArmState>,
    index: usize,
}

impl Arm {
    fn finish(&self) {
        let bit = 1 << self.index;
        if self.state.flags.fetch_and(!bit, Ordering::AcqRel) & bit != 0 {
            self.state.wakers[1 - self.index].wake();
        }
    }
}

impl Drop for Arm {
    fn drop(&mut self) {
        // Also covers a cold histogram rejecting the clone, an unissued delayed clone, and
        // cancellation. No Retry clone owns an arm: Retry only receives the raw request below.
        self.finish();
    }
}

/// A request owns its arm even before Tower's delay and readiness checks have completed.
#[derive(Debug)]
pub(super) struct HedgeRequest {
    request: zn::Request,
    arm: Arm,
}

/// Clone exactly one additional arm; unlike retry payloads, arm ownership is not cloneable.
#[derive(Clone, Copy, Debug)]
pub(super) struct HedgePolicy;

impl hedge::Policy<HedgeRequest> for HedgePolicy {
    fn clone_request(&self, request: &HedgeRequest) -> Option<HedgeRequest> {
        let payload = request.request.clone();
        request.arm.state.flags.fetch_or(2, Ordering::AcqRel);
        Some(HedgeRequest {
            request: payload,
            arm: Arm {
                state: Arc::clone(&request.arm.state),
                index: 1,
            },
        })
    }

    fn can_retry(&self, _request: &HedgeRequest) -> bool {
        true
    }
}

/// Keep each entire retry sequence inside one hedge arm.
#[derive(Debug)]
pub(super) struct ArmService<S> {
    inner: S,
    ready_error: Option<BoxError>,
}

impl<S: Clone> Clone for ArmService<S> {
    fn clone(&self) -> Self {
        // Readiness, including a saved error, belongs to the source instance. Its clone must
        // poll its own inner readiness; Tower moves the ready source into the delayed future.
        Self {
            inner: self.inner.clone(),
            ready_error: None,
        }
    }
}

impl<S> Service<HedgeRequest> for ArmService<S>
where
    S: Service<zn::Request> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ArmFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.ready_error.is_none() {
            if let Err(error) = ready!(self.inner.poll_ready(cx)) {
                // Tower's delayed Oneshot must deliver readiness errors through the arm too,
                // rather than returning early and canceling a still-useful sibling.
                self.ready_error = Some(error.into());
                // A failed readiness check can still hold a ConcurrencyLimit reservation.
                // Drop that service instance without dispatching or discarding its error.
                self.inner = self.inner.clone();
            }
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: HedgeRequest) -> Self::Future {
        let error = self.ready_error.take();
        ArmFuture {
            inner: error.is_none().then(|| self.inner.call(request.request)),
            arm: request.arm,
            error,
        }
    }
}

/// An exhausted arm waits until its sibling finishes or drops, without holding inner capacity.
#[pin_project]
#[derive(Debug)]
pub(super) struct ArmFuture<F> {
    #[pin]
    inner: Option<F>,
    arm: Arm,
    error: Option<BoxError>,
}

impl<F, T, E> Future for ArmFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if let Some(inner) = this.inner.as_mut().as_pin_mut() {
            let result = ready!(inner.poll(cx));
            this.inner.set(None);
            match result {
                Ok(response) => {
                    this.arm.finish();
                    return Poll::Ready(Ok(response));
                }
                Err(error) => *this.error = Some(error.into()),
            }
        }
        if let Some(error) = this.error.take() {
            let mut preferred = this
                .arm
                .state
                .error
                .lock()
                .expect("thread panicked while holding the hedge error mutex");
            if preferred.is_none() || is_not_found(error.as_ref()) {
                *preferred = Some(error);
            }
        }
        this.arm.finish();
        this.arm.state.wakers[this.arm.index].register(cx.waker());
        if this.arm.state.flags.load(Ordering::Acquire) == 0 {
            Poll::Ready(Err(this
                .arm
                .state
                .error
                .lock()
                .expect("thread panicked while holding the hedge error mutex")
                .take()
                .expect("all arms failed before the hedge returns an error")))
        } else {
            Poll::Pending
        }
    }
}
