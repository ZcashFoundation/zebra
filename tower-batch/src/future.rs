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
#[derive(Debug)]
pub struct ResponseFuture<T, E, R>
where
    T: Service<crate::BatchControl<R>>,
{
    #[pin]
    state: ResponseState<T, E, R>,
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
{
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
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
