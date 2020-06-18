//! Future types for the `Batch` middleware.

use super::{error::Closed, message};
use futures_core::ready;
use pin_project::pin_project;
use std::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

/// Future that completes when the batch processing is complete.
#[pin_project]
pub struct ResponseFuture<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
{
    #[pin]
    state: ResponseState<T, E, R>,
}

impl<T, E, R> Debug for ResponseFuture<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
    T::Future: Debug,
    T::Error: Debug,
    E: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseFuture")
            .field("state", &self.state)
            .finish()
    }
}

#[pin_project(project = ResponseStateProj)]
enum ResponseState<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
{
    Failed(Option<E>),
    Rx(#[pin] message::Rx<T::Future, T::Error>),
    Poll(#[pin] T::Future),
}

impl<T, E, R> Debug for ResponseState<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
    T::Future: Debug,
    T::Error: Debug,
    E: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResponseState::Failed(e) => f.debug_tuple("ResponseState::Failed").field(e).finish(),
            ResponseState::Rx(rx) => f.debug_tuple("ResponseState::Rx").field(rx).finish(),
            ResponseState::Poll(fut) => f.debug_tuple("ResponseState::Pool").field(fut).finish(),
        }
    }
}

impl<T, E, R> ResponseFuture<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
{
    pub(crate) fn new(rx: message::Rx<T::Future, T::Error>) -> Self {
        ResponseFuture {
            state: ResponseState::Rx(rx),
        }
    }

    pub(crate) fn failed(err: E) -> Self {
        ResponseFuture {
            state: ResponseState::Failed(Some(err)),
        }
    }
}

impl<S, E2, R> Future for ResponseFuture<S, E2, R>
where
    S: Service<crate::BatchControl<R>>,
    S::Future: Future<Output = Result<S::Response, S::Error>>,
    S::Error: Into<E2>,
    crate::error::Closed: Into<E2>,
{
    type Output = Result<S::Response, E2>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        loop {
            match this.state.as_mut().project() {
                ResponseStateProj::Failed(e) => {
                    return Poll::Ready(Err(e.take().expect("polled after error")));
                }
                ResponseStateProj::Rx(rx) => match ready!(rx.poll(cx)) {
                    Ok(Ok(f)) => this.state.set(ResponseState::Poll(f)),
                    Ok(Err(e)) => return Poll::Ready(Err(e.into())),
                    Err(_) => return Poll::Ready(Err(Closed::new().into())),
                },
                ResponseStateProj::Poll(fut) => return fut.poll(cx).map_err(Into::into),
            }
        }
    }
}
