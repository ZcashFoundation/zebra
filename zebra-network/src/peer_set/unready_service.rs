// Adapted from tower-balance

use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{channel::oneshot, ready};
use tower::Service;

/// A Future that becomes satisfied when an `S`-typed service is ready.
///
/// May fail due to cancelation, i.e. if the service is removed from discovery.
#[pin_project]
#[derive(Debug)]
pub(super) struct UnreadyService<K, S, Req> {
    pub(super) key: Option<K>,
    #[pin]
    pub(super) cancel: oneshot::Receiver<()>,
    pub(super) service: Option<S>,

    pub(super) _req: PhantomData<Req>,
}

pub(super) enum Error<E> {
    Inner(E),
    Canceled,
}

impl<K, S: Service<Req>, Req> Future for UnreadyService<K, S, Req> {
    type Output = Result<(K, S), (K, Error<S::Error>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if let Poll::Ready(Ok(())) = this.cancel.poll(cx) {
            let key = this.key.take().expect("polled after ready");
            return Poll::Ready(Err((key, Error::Canceled)));
        }

        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        //`ready!` returns `Poll::Pending` when the service is unready, and
        // schedules this task for wakeup.
        //
        // `cancel.poll` also schedules this task for wakeup if it is canceled.
        let res = ready!(this
            .service
            .as_mut()
            .expect("poll after ready")
            .poll_ready(cx));

        let key = this.key.take().expect("polled after ready");
        let svc = this.service.take().expect("polled after ready");

        match res {
            Ok(()) => Poll::Ready(Ok((key, svc))),
            Err(e) => Poll::Ready(Err((key, Error::Inner(e)))),
        }
    }
}
