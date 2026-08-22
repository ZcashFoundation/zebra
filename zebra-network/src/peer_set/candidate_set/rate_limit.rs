//! Rate-limiting middleware for candidate peer selection and crawling.

use std::{
    task::{Context, Poll},
    time::Duration,
};

use futures::{future, future::BoxFuture, FutureExt};
use tokio::{
    sync::watch,
    time::{sleep_until, Instant},
};
use tower::Service;

/// A middleware that limits how often the inner service is called.
///
/// While the rate limit applies, calls are skipped: they immediately return
/// `S::Response::default()`, without calling the inner service or consuming
/// any rate-limit budget.
///
/// The limit is claimed for as long as a call is running, so concurrent calls
/// are skipped however long that call takes, and recharged when it finishes,
/// so the interval runs from the end of one call to the start of the next.
///
/// Clones share one rate limit, so only clone this around clones of one inner
/// service — around distinct services it would limit them as if they were one.
#[derive(Clone, Debug)]
pub struct RateLimitBySkipping<S> {
    /// The rate-limited inner service.
    inner: S,

    /// The minimum interval between inner calls.
    interval: Duration,

    /// The next time the inner service is allowed to be called, in a watch
    /// cell shared between clones of this middleware. The cell's closures
    /// make each claim and recharge atomic.
    ///
    /// `None` while a call is in flight, so calls are skipped until it
    /// finishes, even if it runs for longer than `interval`.
    next_allowed: watch::Sender<Option<Instant>>,
}

impl<S> RateLimitBySkipping<S> {
    /// Returns a new [`RateLimitBySkipping`] wrapping `inner`,
    /// limiting calls to one per `interval`.
    pub fn new(inner: S, interval: Duration) -> Self {
        Self {
            inner,
            interval,
            next_allowed: watch::Sender::new(Some(Instant::now())),
        }
    }
}

impl<S, Request> Service<Request> for RateLimitBySkipping<S>
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
        let mut claimed = false;
        self.next_allowed.send_if_modified(|next_allowed| {
            match *next_allowed {
                // While rate-limited, or while another call is in flight,
                // skip the call and return the default response immediately.
                None => false,
                Some(allowed_at) if Instant::now() < allowed_at => false,
                // Claim the rate limit for as long as the call runs, so
                // concurrent calls are skipped even if it takes longer than
                // `interval`.
                Some(_) => {
                    *next_allowed = None;
                    claimed = true;
                    true
                }
            }
        });

        if !claimed {
            return future::ready(Ok(S::Response::default())).boxed();
        }

        let call = self.inner.call(request);
        let next_allowed = self.next_allowed.clone();
        let interval = self.interval;

        async move {
            let result = call.await;

            // Recharge the rate limit from the completion time, so the
            // interval runs from the end of this call.
            //
            // # Correctness
            //
            // This must also happen when the call fails: the claim above is
            // an open-ended `None`, so leaving it in place after an error
            // would skip every later call for the lifetime of the process.
            next_allowed
                .send_modify(|next_allowed| *next_allowed = Some(Instant::now() + interval));

            result
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

    /// The next free pacing slot, in a watch cell shared between clones of
    /// this middleware. The cell's closures make each slot reservation and
    /// recharge atomic.
    next_allowed: watch::Sender<Instant>,
}

impl<S, Response> RateLimitOnYield<S, Response> {
    /// Returns a new [`RateLimitOnYield`] wrapping `inner`, limiting the
    /// responses that `charges` to one per `interval`.
    pub fn new(inner: S, interval: Duration, charges: fn(&Response) -> bool) -> Self {
        Self {
            inner,
            interval,
            charges,
            next_allowed: watch::Sender::new(Instant::now()),
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
                let mut slot = Instant::now();
                next_allowed.send_modify(|next_allowed| {
                    if *next_allowed > slot {
                        slot = *next_allowed;
                    }
                    *next_allowed = slot + interval;
                });

                sleep_until(slot).await;

                // Recharge from the actual wake time, like the replaced
                // manual timer, so scheduler wake-up latency doesn't
                // accumulate as a pacing deficit. Only ever extend the shared
                // time, so concurrent slot reservations are never undone.
                next_allowed.send_modify(|next_allowed| {
                    let recharge = Instant::now() + interval;
                    if recharge > *next_allowed {
                        *next_allowed = recharge;
                    }
                });
            }

            Ok(response)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use tower::{service_fn, ServiceExt};

    use super::*;

    /// The interval used by the tests, long enough that it never elapses
    /// on its own during a test run.
    const TEST_INTERVAL: Duration = Duration::from_secs(30);

    /// Test that a call running for longer than the interval still skips
    /// concurrent calls, rather than letting a second call through once the
    /// interval has elapsed.
    #[tokio::test(start_paused = true)]
    async fn long_call_skips_concurrent_calls() {
        let _init_guard = zebra_test::init();

        let calls = Arc::new(AtomicUsize::new(0));

        let inner = {
            let calls = calls.clone();
            service_fn(move |()| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Run for much longer than the rate-limit interval.
                    tokio::time::sleep(TEST_INTERVAL * 4).await;
                    Ok::<bool, std::convert::Infallible>(true)
                }
            })
        };

        let service = RateLimitBySkipping::new(inner, TEST_INTERVAL);

        // Start a long call, and let it reach the inner service.
        let long_call = tokio::spawn(service.clone().oneshot(()));
        tokio::task::yield_now().await;

        // Wait past the interval, while the first call is still running.
        tokio::time::sleep(TEST_INTERVAL * 2).await;

        let skipped = service
            .clone()
            .oneshot(())
            .await
            .expect("skipped calls return the default response");

        assert!(
            !skipped,
            "a call made while another is in flight must be skipped, \
             even after the interval has elapsed",
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the inner service must only be called once while a call is in flight",
        );

        assert!(long_call.await.expect("call task should not panic").is_ok());
    }

    /// Test that a failed call recharges the rate limit, instead of leaving
    /// the open-ended in-flight claim in place forever.
    #[tokio::test(start_paused = true)]
    async fn failed_call_recharges_the_rate_limit() {
        let _init_guard = zebra_test::init();

        let service = RateLimitBySkipping::new(
            service_fn(|()| async { Err::<bool, &'static str>("inner service error") }),
            TEST_INTERVAL,
        );

        service
            .clone()
            .oneshot(())
            .await
            .expect_err("the inner error should be returned");

        // After the interval, calls are allowed again.
        tokio::time::sleep(TEST_INTERVAL * 2).await;

        service
            .clone()
            .oneshot(())
            .await
            .expect_err("a later call should reach the inner service, not be skipped forever");
    }
}
