//! Future types for the `FairBuffer` middleware.

use std::{
    future::Future,
    hash::Hash,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll},
};

use tokio::time::Instant;

use pin_project::pin_project;

use super::{counts::RecentRequestCosts, error::Closed, message};

/// Future that completes when the fair buffered service responds to the
/// submitted request.
///
/// Once the worker dispatches the request, the future also measures how long
/// the inner service takes to respond, and records that response time
/// against the caller's recent request cost — so a caller's priority depends
/// on how expensive its requests are to serve, not just how many it sends.
#[pin_project]
pub struct ResponseFuture<K, T> {
    #[pin]
    state: ResponseState<T>,

    /// The caller's cost key, taken when the response time is recorded.
    ///
    /// `None` for internal requests, failed requests, and after recording.
    key: Option<K>,

    /// The caller costs to record the response time into.
    costs: Option<Arc<Mutex<RecentRequestCosts<K>>>>,

    /// When the worker dispatched the request to the inner service.
    dispatched: Option<Instant>,
}

#[pin_project(project = ResponseStateProj)]
#[derive(Debug)]
enum ResponseState<T> {
    Failed(Option<crate::BoxError>),
    Rx(#[pin] message::Rx<T>),
    Poll(#[pin] T),
}

impl<K, T> ResponseFuture<K, T> {
    pub(crate) fn new(
        rx: message::Rx<T>,
        key: Option<K>,
        costs: Arc<Mutex<RecentRequestCosts<K>>>,
    ) -> Self {
        ResponseFuture {
            state: ResponseState::Rx(rx),
            key,
            costs: Some(costs),
            dispatched: None,
        }
    }

    pub(crate) fn failed(err: crate::BoxError) -> Self {
        ResponseFuture {
            state: ResponseState::Failed(Some(err)),
            key: None,
            costs: None,
            dispatched: None,
        }
    }
}

impl<K, T> std::fmt::Debug for ResponseFuture<K, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseFuture").finish_non_exhaustive()
    }
}

impl<K, F, T, E> Future for ResponseFuture<K, F>
where
    K: Eq + Hash,
    F: Future<Output = Result<T, E>>,
    E: Into<crate::BoxError>,
{
    type Output = Result<T, crate::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // This loop ensures that the task is scheduled as required, because it
        // only returns Pending when another future returns Pending.
        loop {
            match this.state.as_mut().project() {
                ResponseStateProj::Failed(e) => {
                    return Poll::Ready(Err(e.take().expect("polled after error")));
                }
                ResponseStateProj::Rx(rx) => match ready!(rx.poll(cx)) {
                    Ok(Ok(f)) => {
                        // The worker has dispatched the request: the inner
                        // service's response time starts now.
                        *this.dispatched = Some(Instant::now());
                        this.state.set(ResponseState::Poll(f));
                    }
                    Ok(Err(e)) => return Poll::Ready(Err(e)),
                    Err(_) => return Poll::Ready(Err(Closed::new().into())),
                },
                ResponseStateProj::Poll(fut) => {
                    let result = ready!(fut.poll(cx));

                    // Record the response time against the caller's recent
                    // cost — for successes and errors alike: the inner
                    // service did the work either way. `take()` makes the
                    // recording happen at most once.
                    if let (Some(key), Some(costs), Some(dispatched)) =
                        (this.key.take(), this.costs.take(), *this.dispatched)
                    {
                        costs
                            .lock()
                            .expect("a caller or the worker panicked while holding the fair buffer costs lock")
                            .record_response_time(key, dispatched.elapsed());
                    }

                    return Poll::Ready(result.map_err(Into::into));
                }
            }
        }
    }
}
