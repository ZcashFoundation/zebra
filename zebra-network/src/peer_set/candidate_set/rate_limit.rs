//! Rate-limiting middleware for candidate peer selection and crawling.
//!
//! These middlewares replace the manual `min_next_handshake` and
//! `min_next_crawl` timers that used to live in the candidate set.

use std::{
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use futures::{future, future::BoxFuture, FutureExt};
use tokio::time::{sleep_until, Instant};
use tower::Service;

/// A middleware that limits how often the inner service is called.
///
/// While the rate limit applies, calls are skipped: they immediately return
/// `S::Response::default()`, without calling the inner service or consuming
/// any rate-limit budget.
///
/// The limit is claimed when a call starts, so concurrent calls are skipped
/// while one is running, and recharged when a call completes successfully,
/// so the interval runs from the end of one successful call to the start of
/// the next. This matches the replaced `min_next_crawl` timer, which was
/// updated after each crawl attempt finished.
#[derive(Clone, Debug)]
pub struct SkipRateLimit<S> {
    /// The rate-limited inner service.
    inner: S,

    /// The minimum interval between inner calls.
    interval: Duration,

    /// The next time the inner service is allowed to be called,
    /// shared between clones of this middleware.
    next_allowed: Arc<Mutex<Instant>>,
}

impl<S> SkipRateLimit<S> {
    /// Returns a new [`SkipRateLimit`] wrapping `inner`,
    /// limiting calls to one per `interval`.
    pub fn new(inner: S, interval: Duration) -> Self {
        Self {
            inner,
            interval,
            next_allowed: Arc::new(Mutex::new(Instant::now())),
        }
    }
}

impl<S, Request> Service<Request> for SkipRateLimit<S>
where
    S: Service<Request>,
    S::Response: Default + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Skipped calls reserve inner readiness without using it, which is
        // fine here, because the inner crawl service is always ready.
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        {
            let mut next_allowed = self
                .next_allowed
                .lock()
                .expect("mutex should be unpoisoned");
            let now = Instant::now();

            if now < *next_allowed {
                // While rate-limited, skip the call and return the default
                // response immediately.
                return future::ready(Ok(S::Response::default())).boxed();
            }

            // Claim the rate limit until the call completes, so concurrent
            // calls are skipped while this one is running.
            *next_allowed = now + self.interval;
        }

        let response = self.inner.call(request);
        let next_allowed = self.next_allowed.clone();
        let interval = self.interval;

        async move {
            let response = response.await?;

            // Recharge the rate limit from the completion time, so the
            // interval runs from the end of this call, matching the replaced
            // manual timer. Errors skip this recharge, but they don't refund
            // the claim above; callers treat inner errors as permanent.
            *next_allowed.lock().expect("mutex should be unpoisoned") = Instant::now() + interval;

            Ok(response)
        }
        .boxed()
    }
}

/// A middleware that rate-limits the responses that yield a result,
/// as decided by the `charges` predicate.
///
/// Every call is forwarded to the inner service immediately. If `charges`
/// returns `true` for the response, this middleware reserves the earliest
/// free pacing slot, waits until that slot's time, and only then returns the
/// response. Responses that don't charge are returned immediately, without
/// consuming any rate-limit budget.
///
/// Because slots are reserved atomically, concurrent yielding responses are
/// returned at least `interval` apart, in slot reservation order.
#[derive(Clone, Debug)]
pub struct RateLimitOnYield<S, Response> {
    /// The inner service.
    inner: S,

    /// The minimum interval between yielding responses.
    interval: Duration,

    /// Returns `true` if a response yields a result,
    /// consuming rate-limit budget.
    charges: fn(&Response) -> bool,

    /// The next free pacing slot, shared between clones of this middleware.
    next_allowed: Arc<Mutex<Instant>>,
}

impl<S, Response> RateLimitOnYield<S, Response> {
    /// Returns a new [`RateLimitOnYield`] wrapping `inner`, limiting the
    /// responses that `charges` to one per `interval`.
    pub fn new(inner: S, interval: Duration, charges: fn(&Response) -> bool) -> Self {
        Self {
            inner,
            interval,
            charges,
            next_allowed: Arc::new(Mutex::new(Instant::now())),
        }
    }
}

impl<S, Request, Response> Service<Request> for RateLimitOnYield<S, Response>
where
    S: Service<Request, Response = Response>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    Response: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let response = self.inner.call(request);
        let charges = self.charges;
        let interval = self.interval;
        let next_allowed = self.next_allowed.clone();

        async move {
            let response = response.await?;

            if charges(&response) {
                // Reserve the earliest free pacing slot, then sleep until the
                // slot's time. Reserving before sleeping means concurrent
                // yields wake at least `interval` apart.
                let slot = {
                    let mut next_allowed = next_allowed.lock().expect("mutex should be unpoisoned");
                    let now = Instant::now();
                    let slot = if *next_allowed > now {
                        *next_allowed
                    } else {
                        now
                    };
                    *next_allowed = slot + interval;
                    slot
                };

                sleep_until(slot).await;

                // Recharge from the actual wake time, like the replaced
                // manual timer, so scheduler wake-up latency doesn't
                // accumulate as a pacing deficit. Only ever extend the shared
                // time, so concurrent slot reservations are never undone.
                {
                    let mut next_allowed = next_allowed.lock().expect("mutex should be unpoisoned");
                    let recharge = Instant::now() + interval;
                    if recharge > *next_allowed {
                        *next_allowed = recharge;
                    }
                }
            }

            Ok(response)
        }
        .boxed()
    }
}
