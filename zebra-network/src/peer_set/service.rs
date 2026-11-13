//! External ownership of the network's buffered service and background tasks.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::task::AbortHandle;
use tower::{buffer::Buffer, util::BoxService, Service};

use crate::{BoxError, Request, Response};

/// A cloneable, buffered service for outbound network requests.
///
/// The network stays alive while an owning handle or an unfinished [`NetworkResponseFuture`]
/// exists. Releasing the last owner aborts the worker and background tasks, releasing the
/// listener and peer clients while the runtime is still running. Internal crawler and polling
/// tasks use the underlying buffer without sharing this shutdown ownership.
#[derive(Clone)]
pub struct NetworkService {
    inner: Buffer<BoxService<Request, Response, BoxError>, Request>,
    tasks: Arc<NetworkTasks>,
}

// The boxed inner service is not `Debug`.
impl fmt::Debug for NetworkService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkService")
            .field("tasks", &self.tasks)
            .finish_non_exhaustive()
    }
}

/// An outbound response that keeps its [`NetworkService`] alive until completion or cancellation.
///
/// This also preserves ownership when [`tower::ServiceExt::oneshot`] consumes the last service
/// handle. Background handles returned by [`NetworkService::background_service`] do not create
/// owning response futures.
#[pin_project]
pub struct NetworkResponseFuture {
    #[pin]
    inner: <Buffer<BoxService<Request, Response, BoxError>, Request> as Service<Request>>::Future,
    tasks: Option<Arc<NetworkTasks>>,
}

impl Future for NetworkResponseFuture {
    type Output = Result<Response, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let response = this.inner.poll(cx);
        if response.is_ready() {
            drop(this.tasks.take());
        }
        response
    }
}

#[derive(Debug)]
struct NetworkTasks(Vec<AbortHandle>);

impl Drop for NetworkTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

impl NetworkService {
    /// Takes ownership of the existing buffer and its tasks without adding another buffer.
    pub(crate) fn new(
        inner: Buffer<BoxService<Request, Response, BoxError>, Request>,
        tasks: Vec<AbortHandle>,
    ) -> Self {
        Self {
            inner,
            tasks: Arc::new(NetworkTasks(tasks)),
        }
    }

    /// Returns a buffered handle that does not keep the network running.
    ///
    /// Use this handle in services retained by the network, including inbound handlers and
    /// their downloaders, to avoid a shutdown ownership cycle. Keep a [`NetworkService`]
    /// handle in an external owner for as long as the network is needed.
    ///
    /// After the last owning handle and unfinished response are released, requests through
    /// this handle fail even if background handles remain. Neither this handle nor its response
    /// futures keep the network alive. This does not add another buffer or worker.
    pub fn background_service(&self) -> Buffer<BoxService<Request, Response, BoxError>, Request> {
        self.inner.clone()
    }

    /// Registers tasks during initialization, before sharing external handles.
    pub(crate) fn extend_tasks(&mut self, tasks: impl IntoIterator<Item = AbortHandle>) {
        Arc::get_mut(&mut self.tasks)
            .expect("network tasks are registered before external handles are shared")
            .0
            .extend(tasks);
    }
}

impl Service<Request> for NetworkService {
    type Response = Response;
    type Error = BoxError;
    type Future = NetworkResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        NetworkResponseFuture {
            inner: self.inner.call(request),
            tasks: Some(self.tasks.clone()),
        }
    }
}
