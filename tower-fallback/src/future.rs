//! Future types for the `Fallback` middleware.

// TODO: remove this lint exception after upgrading to pin-project 1.0.11 or later (#2355)
#![allow(dead_code)]

use std::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::ready;
use pin_project::pin_project;
use tower::Service;

use crate::BoxedError;

/// Future that completes either with the first service's successful response, or
/// with the second service's response.
#[pin_project]
pub struct ResponseFuture<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    S2::Error: Into<BoxedError>,
{
    #[pin]
    state: ResponseState<S1, S2, Request>,
}

#[pin_project(project_replace, project = ResponseStateProj)]
enum ResponseState<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request>,
    S2::Error: Into<BoxedError>,
{
    PollResponse1 {
        #[pin]
        fut: S1::Future,
        req: Request,
        svc2: S2,
    },
    PollReady2 {
        req: Request,
        svc2: S2,
    },
    PollResponse2 {
        #[pin]
        fut: S2::Future,
    },
    // Placeholder value to swap into the pin projection of the enum so we can take ownership of the fields.
    Tmp,
}

impl<S1, S2, Request> ResponseFuture<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    S2::Error: Into<BoxedError>,
{
    pub(crate) fn new(fut: S1::Future, req: Request, svc2: S2) -> Self {
        ResponseFuture {
            state: ResponseState::PollResponse1 { fut, req, svc2 },
        }
    }
}

impl<S1, S2, Request> Future for ResponseFuture<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    S2::Error: Into<BoxedError>,
{
    type Output = Result<<S1 as Service<Request>>::Response, BoxedError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // This loop ensures that the task is scheduled as required, because it
        // only returns Pending when a future or service returns Pending.
        loop {
            match this.state.as_mut().project() {
                ResponseStateProj::PollResponse1 { fut, .. } => match ready!(fut.poll(cx)) {
                    Ok(rsp) => return Poll::Ready(Ok(rsp)),
                    Err(_) => {
                        tracing::debug!("got error from svc1, retrying on svc2");
                        if let __ResponseStateProjectionOwned::PollResponse1 { req, svc2, .. } =
                            this.state.as_mut().project_replace(ResponseState::Tmp)
                        {
                            this.state.set(ResponseState::PollReady2 { req, svc2 });
                        } else {
                            unreachable!();
                        }
                    }
                },
                ResponseStateProj::PollReady2 { svc2, .. } => match ready!(svc2.poll_ready(cx)) {
                    Err(e) => return Poll::Ready(Err(e.into())),
                    Ok(()) => {
                        if let __ResponseStateProjectionOwned::PollReady2 { mut svc2, req } =
                            this.state.as_mut().project_replace(ResponseState::Tmp)
                        {
                            this.state.set(ResponseState::PollResponse2 {
                                fut: svc2.call(req),
                            });
                        } else {
                            unreachable!();
                        }
                    }
                },
                ResponseStateProj::PollResponse2 { fut } => {
                    return fut.poll(cx).map_err(Into::into)
                }
                ResponseStateProj::Tmp => unreachable!(),
            }
        }
    }
}

impl<S1, S2, Request> Debug for ResponseFuture<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    Request: Debug,
    S1::Future: Debug,
    S2: Debug,
    S2::Future: Debug,
    S2::Error: Into<BoxedError>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseFuture")
            .field("state", &self.state)
            .finish()
    }
}

impl<S1, S2, Request> Debug for ResponseState<S1, S2, Request>
where
    S1: Service<Request>,
    S2: Service<Request, Response = <S1 as Service<Request>>::Response>,
    Request: Debug,
    S1::Future: Debug,
    S2: Debug,
    S2::Future: Debug,
    S2::Error: Into<BoxedError>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResponseState::PollResponse1 { fut, req, svc2 } => f
                .debug_struct("ResponseState::PollResponse1")
                .field("fut", fut)
                .field("req", req)
                .field("svc2", svc2)
                .finish(),
            ResponseState::PollReady2 { req, svc2 } => f
                .debug_struct("ResponseState::PollReady2")
                .field("req", req)
                .field("svc2", svc2)
                .finish(),
            ResponseState::PollResponse2 { fut } => f
                .debug_struct("ResponseState::PollResponse2")
                .field("fut", fut)
                .finish(),
            ResponseState::Tmp => unreachable!(),
        }
    }
}
