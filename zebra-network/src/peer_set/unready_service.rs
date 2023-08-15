//! Services that are busy or newly created.
//!
//! The [`UnreadyService`] implementation is adapted from the one in [tower::Balance][tower-balance].
//!
//! [tower-balance]: https://github.com/tower-rs/tower/tree/master/tower/src/balance

use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{channel::oneshot, ready};
use tower::Service;

use crate::peer_set::set::CancelClientWork;

#[cfg(test)]
mod tests;

/// A Future that becomes satisfied when an `S`-typed service is ready.
///
/// May fail due to cancellation, i.e. if the service is removed from discovery.
#[pin_project]
#[derive(Debug)]
pub(super) struct UnreadyService<K, S, Req> {
    /// The key used to lookup `service`.
    pub(super) key: Option<K>,

    /// A oneshot used to cancel the request the `service` is currently working on, if any.
    #[pin]
    pub(super) cancel: oneshot::Receiver<CancelClientWork>,

    /// The `service` that is busy (or newly created).
    pub(super) service: Option<S>,

    /// Dropping `service` might drop a request.
    /// This [`PhantomData`] tells the Rust compiler to do a drop check for `Req`.
    pub(super) _req: PhantomData<Req>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Error<E> {
    Inner(E),
    Canceled,
    CancelHandleDropped(oneshot::Canceled),
}

impl<K, S: Service<Req>, Req> Future for UnreadyService<K, S, Req> {
    type Output = Result<(K, S), (K, Error<S::Error>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if let Poll::Ready(oneshot_result) = this.cancel.poll(cx) {
            let key = this.key.take().expect("polled after ready");

            // # Correctness
            //
            // Return an error if the service is explicitly canceled,
            // or its cancel handle is dropped, implicitly cancelling it.
            match oneshot_result {
                Ok(CancelClientWork) => return Poll::Ready(Err((key, Error::Canceled))),
                Err(canceled_error) => {
                    return Poll::Ready(Err((key, Error::CancelHandleDropped(canceled_error))))
                }
            }
        }

        // # Correctness
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        //`ready!` returns `Poll::Pending` when the service is unready, and
        // the inner `poll_ready` schedules this task for wakeup.
        //
        // `cancel.poll` also schedules this task for wakeup if it is canceled.
        let res = ready!(this
            .service
            .as_mut()
            .expect("polled after ready")
            .poll_ready(cx));

        let key = this.key.take().expect("polled after ready");
        let svc = this.service.take().expect("polled after ready");

        match res {
            Ok(()) => Poll::Ready(Ok((key, svc))),
            Err(e) => Poll::Ready(Err((key, Error::Inner(e)))),
        }
    }
}
