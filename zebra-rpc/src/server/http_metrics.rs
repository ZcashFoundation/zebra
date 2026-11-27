//! HTTP-level connection metrics middleware for the JSON-RPC server.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use hyper::{header, StatusCode};
use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::{Layer, Service};

/// Matches `jsonrpsee`'s default `MAX_CONNECTIONS` limit so [`HttpMetricsLayer`] and
/// `jsonrpsee::server::Server` enforce the same concurrent connection permit budget.
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 100;

/// [`Layer`] that wraps jsonrpsee's HTTP service in [`HttpMetricsMiddleware`].
#[derive(Clone, Debug)]
pub(super) struct HttpMetricsLayer {
    permits: Arc<Semaphore>,
}

impl HttpMetricsLayer {
    /// Creates an HTTP metrics layer enforcing at most `max_connections` active connection permits.
    pub(super) fn new(max_connections: u32) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(
                usize::try_from(max_connections).expect("u32 fits in usize"),
            )),
        }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpMetricsMiddleware {
            service,
            permits: self.permits.clone(),
        }
    }
}

/// Tower middleware that records JSON-RPC server connection permit usage and HTTP 429 rejections.
///
/// Layered inside [`HttpRequestMiddleware`](super::http_request_compatibility::HttpRequestMiddleware)
/// so unauthenticated 401s and oversized request body errors never touch connection permits.
#[derive(Clone, Debug)]
pub(super) struct HttpMetricsMiddleware<S> {
    service: S,
    permits: Arc<Semaphore>,
}

impl<S> Service<HttpRequest<HttpBody>> for HttpMetricsMiddleware<S>
where
    S: Service<HttpRequest<HttpBody>, Response = HttpResponse<HttpBody>> + Clone + Send + 'static,
    S::Error: Into<BoxError> + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse<HttpBody>;
    type Error = BoxError;
    type Future = HttpMetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: HttpRequest<HttpBody>) -> Self::Future {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            metrics::counter!("rpc.connections.rejected.total").increment(1);
            return HttpMetricsFuture::Rejected;
        };

        metrics::gauge!("rpc.connections.active").increment(1.0);
        let tracked = TrackedConnectionPermit { _permit: permit };
        let inner = Box::pin(self.service.call(request));

        HttpMetricsFuture::Admitted {
            inner: Some(inner),
            _permit: Some(tracked),
        }
    }
}

/// Holds an HTTP connection semaphore permit and decrements `rpc.connections.active` before the
/// permit is released back to `Semaphore`.
pub(super) struct TrackedConnectionPermit {
    _permit: OwnedSemaphorePermit,
}

impl Drop for TrackedConnectionPermit {
    fn drop(&mut self) {
        // Decrement before `_permit` drops so another task cannot acquire the released permit and
        // temporarily bump `rpc.connections.active` above `max_connections`.
        metrics::gauge!("rpc.connections.active").decrement(1.0);
    }
}

/// Returns the HTTP 429 response matching `jsonrpsee`'s connection-limit rejection.
fn too_many_requests_response() -> HttpResponse<HttpBody> {
    HttpResponse::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(HttpBody::from(
            "Too many connections. Please try again later.",
        ))
        .expect("static HTTP 429 response is valid")
}

/// Future returned by [`HttpMetricsMiddleware`].
pub(super) enum HttpMetricsFuture<F> {
    Admitted {
        // Field declaration order matters: `inner` drops before `_permit`, so jsonrpsee releases
        // its inner connection state before we decrement the gauge and release the semaphore permit.
        inner: Option<Pin<Box<F>>>,
        _permit: Option<TrackedConnectionPermit>,
    },
    Rejected,
}

impl<F, E> Future for HttpMetricsFuture<F>
where
    F: Future<Output = Result<HttpResponse<HttpBody>, E>>,
    E: Into<BoxError>,
{
    type Output = Result<HttpResponse<HttpBody>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this {
            Self::Rejected => Poll::Ready(Ok(too_many_requests_response())),
            Self::Admitted { inner, _permit } => {
                let inner_fut = inner.as_mut().expect("future polled after completion");
                match inner_fut.as_mut().poll(cx) {
                    Poll::Ready(result) => {
                        if let Ok(response) = &result {
                            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                                metrics::counter!("rpc.connections.rejected.total").increment(1);
                            }
                        }
                        *inner = None;
                        *_permit = None;
                        Poll::Ready(result.map_err(Into::into))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::future;
    use hyper::StatusCode;
    use tokio::sync::{oneshot, Notify};
    use tower::{service_fn, Layer, ServiceExt};

    use super::*;
    use crate::metrics::tests::TestMetricsRecorder;

    #[tokio::test]
    async fn pending_request_tracks_active_permit_until_completion() {
        let _init_guard = zebra_test::init();
        let recorder = TestMetricsRecorder::default();
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let started = Arc::new(Notify::new());
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let started_clone = started.clone();

        let inner = service_fn(move |_req: HttpRequest<HttpBody>| {
            let started = started_clone.clone();
            let release_rx = release_rx.clone();
            async move {
                started.notify_one();
                if let Some(rx) = release_rx.lock().await.take() {
                    let _ = rx.await;
                }
                Ok::<_, BoxError>(HttpResponse::new(HttpBody::empty()))
            }
        });

        let mut service = HttpMetricsLayer::new(1).layer(inner);
        let request = HttpRequest::new(HttpBody::empty());
        let handle = tokio::spawn(service.ready().await.unwrap().call(request));

        started.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));

        let _ = release_tx.send(());
        let response = handle.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
        assert_eq!(
            recorder.counter("rpc.connections.rejected.total", &[]),
            None
        );
    }

    #[tokio::test]
    async fn dropped_pending_request_decrements_active_permit() {
        let _init_guard = zebra_test::init();
        let recorder = TestMetricsRecorder::default();
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let started = Arc::new(Notify::new());
        let started_clone = started.clone();

        let inner = service_fn(move |_req: HttpRequest<HttpBody>| {
            let started = started_clone.clone();
            async move {
                started.notify_one();
                future::pending::<Result<HttpResponse<HttpBody>, BoxError>>().await
            }
        });

        let mut service = HttpMetricsLayer::new(1).layer(inner);
        let fut = service
            .ready()
            .await
            .unwrap()
            .call(HttpRequest::new(HttpBody::empty()));
        let handle = tokio::spawn(fut);

        started.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));

        handle.abort();
        let _ = handle.await;
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
    }

    #[tokio::test]
    async fn immediate_429_increments_rejected_without_touching_active() {
        let _init_guard = zebra_test::init();
        let recorder = TestMetricsRecorder::default();
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let started = Arc::new(Notify::new());
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let started_clone = started.clone();

        let inner = service_fn(move |_req: HttpRequest<HttpBody>| {
            let started = started_clone.clone();
            let release_rx = release_rx.clone();
            async move {
                started.notify_one();
                if let Some(rx) = release_rx.lock().await.take() {
                    let _ = rx.await;
                }
                Ok::<_, BoxError>(HttpResponse::new(HttpBody::empty()))
            }
        });

        let mut service = HttpMetricsLayer::new(1).layer(inner);
        let parked = tokio::spawn(
            service
                .ready()
                .await
                .unwrap()
                .call(HttpRequest::new(HttpBody::empty())),
        );
        started.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(HttpRequest::new(HttpBody::empty()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            recorder.counter("rpc.connections.rejected.total", &[]),
            Some(1)
        );
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(1.0));
        assert_eq!(recorder.gauge_max("rpc.connections.active", &[]), Some(1.0));

        let _ = release_tx.send(());
        let first_response = parked.await.unwrap().unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
    }

    #[tokio::test]
    async fn synchronous_first_poll_ok_leaves_connections_clean() {
        let _init_guard = zebra_test::init();
        let recorder = TestMetricsRecorder::default();
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let inner = service_fn(|_req: HttpRequest<HttpBody>| {
            future::ready(Ok::<_, BoxError>(HttpResponse::new(HttpBody::empty())))
        });

        let mut service = HttpMetricsLayer::new(1).layer(inner);
        let response = service
            .ready()
            .await
            .unwrap()
            .call(HttpRequest::new(HttpBody::empty()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            recorder.counter("rpc.connections.rejected.total", &[]),
            None
        );
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
        assert_eq!(recorder.gauge_max("rpc.connections.active", &[]), Some(1.0));
    }

    struct WithLocalRecorder<F> {
        recorder: Arc<TestMetricsRecorder>,
        future: Option<Pin<Box<F>>>,
    }

    impl<F: Future> Future for WithLocalRecorder<F> {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let future = this
                .future
                .as_mut()
                .expect("WithLocalRecorder polled after completion");
            metrics::with_local_recorder(this.recorder.as_ref(), || future.as_mut().poll(cx))
        }
    }

    impl<F> Drop for WithLocalRecorder<F> {
        fn drop(&mut self) {
            if let Some(future) = self.future.take() {
                metrics::with_local_recorder(self.recorder.as_ref(), || drop(future));
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overlapping_completion_and_admission_never_exceeds_max_connections() {
        let _init_guard = zebra_test::init();
        let recorder = Arc::new(TestMetricsRecorder::default());
        let layer = HttpMetricsLayer::new(1);

        let inner = service_fn(|_req: HttpRequest<HttpBody>| async {
            tokio::task::yield_now().await;
            Ok::<_, BoxError>(HttpResponse::new(HttpBody::empty()))
        });

        const WORKERS: usize = 4;
        const REQUESTS_PER_WORKER: usize = 250;
        let barrier = Arc::new(tokio::sync::Barrier::new(WORKERS));

        let mut handles = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let mut service = layer.layer(inner);
            let recorder = recorder.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut ok_count = 0u64;
                let mut rejected_count = 0u64;

                for _ in 0..REQUESTS_PER_WORKER {
                    service.ready().await.unwrap();
                    let fut = metrics::with_local_recorder(recorder.as_ref(), || {
                        service.call(HttpRequest::new(HttpBody::empty()))
                    });
                    let response = WithLocalRecorder {
                        recorder: recorder.clone(),
                        future: Some(Box::pin(fut)),
                    }
                    .await
                    .unwrap();
                    match response.status() {
                        StatusCode::OK => ok_count += 1,
                        StatusCode::TOO_MANY_REQUESTS => rejected_count += 1,
                        other => panic!("unexpected status: {other}"),
                    }
                }

                (ok_count, rejected_count)
            }));
        }

        let mut total_ok = 0u64;
        let mut total_rejected = 0u64;
        for handle in handles {
            let (ok_count, rejected_count) = handle.await.unwrap();
            total_ok += ok_count;
            total_rejected += rejected_count;
        }

        assert_eq!(
            total_ok + total_rejected,
            (WORKERS * REQUESTS_PER_WORKER) as u64
        );
        assert!(total_ok > 0);
        assert!(total_rejected > 0);
        assert_eq!(recorder.gauge_max("rpc.connections.active", &[]), Some(1.0));
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), Some(0.0));
        assert_eq!(
            recorder.counter("rpc.connections.rejected.total", &[]),
            Some(total_rejected)
        );
    }
}
