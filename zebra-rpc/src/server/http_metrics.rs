//! HTTP-level connection metrics middleware for the JSON-RPC server.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use tower::{Layer, Service};

/// [`Layer`] that wraps jsonrpsee's HTTP service in [`HttpMetricsMiddleware`].
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct HttpMetricsLayer;

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpMetricsMiddleware { service }
    }
}

/// Tower middleware that records JSON-RPC server connection permit usage and HTTP 429 rejections.
///
/// Layered inside [`HttpRequestMiddleware`](super::http_request_compatibility::HttpRequestMiddleware)
/// so unauthenticated 401s and oversized request body errors never touch connection permits.
#[derive(Clone, Debug)]
pub(super) struct HttpMetricsMiddleware<S> {
    service: S,
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
        HttpMetricsFuture {
            inner: Box::pin(self.service.call(request)),
            permit_tracked: false,
        }
    }
}

/// Future returned by [`HttpMetricsMiddleware`].
///
/// Uses poll-once-before-incrementing: `jsonrpsee`'s `TowerServiceNoHttp::call` calls
/// `conn_guard.try_acquire()` synchronously and resolves HTTP 429 rejections (and fast cached
/// responses) on the first poll, whereas in-flight requests holding a permit return
/// `Poll::Pending`. Polling the inner future first prevents 429 floods from temporarily spiking
/// `rpc.connections.active` above `max_connections`, while decrementing in `Drop` keeps the gauge
/// accurate on client disconnects.
pub(super) struct HttpMetricsFuture<F> {
    inner: Pin<Box<F>>,
    permit_tracked: bool,
}

impl<F, E> Future for HttpMetricsFuture<F>
where
    F: Future<Output = Result<HttpResponse<HttpBody>, E>>,
    E: Into<BoxError>,
{
    type Output = Result<HttpResponse<HttpBody>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.inner.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if let Ok(response) = &result {
                    if response.status() == hyper::StatusCode::TOO_MANY_REQUESTS {
                        metrics::counter!("rpc.connections.rejected.total").increment(1);
                    }
                }
                if this.permit_tracked {
                    this.permit_tracked = false;
                    metrics::gauge!("rpc.connections.active").decrement(1.0);
                }
                Poll::Ready(result.map_err(Into::into))
            }
            Poll::Pending => {
                if !this.permit_tracked {
                    this.permit_tracked = true;
                    metrics::gauge!("rpc.connections.active").increment(1.0);
                }
                Poll::Pending
            }
        }
    }
}

impl<F> Drop for HttpMetricsFuture<F> {
    fn drop(&mut self) {
        if self.permit_tracked {
            self.permit_tracked = false;
            metrics::gauge!("rpc.connections.active").decrement(1.0);
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

        let mut service = HttpMetricsLayer.layer(inner);
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

        let mut service = HttpMetricsLayer.layer(inner);
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

        let inner = service_fn(|_req: HttpRequest<HttpBody>| {
            future::ready(Ok::<_, BoxError>(
                HttpResponse::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .body(HttpBody::from(
                        "Too many connections. Please try again later.",
                    ))
                    .unwrap(),
            ))
        });

        let mut service = HttpMetricsLayer.layer(inner);
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
        assert_eq!(recorder.gauge("rpc.connections.active", &[]), None);
        assert_eq!(recorder.gauge_max("rpc.connections.active", &[]), None);
    }

    #[tokio::test]
    async fn synchronous_first_poll_ok_leaves_connections_clean() {
        let _init_guard = zebra_test::init();
        let recorder = TestMetricsRecorder::default();
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let inner = service_fn(|_req: HttpRequest<HttpBody>| {
            future::ready(Ok::<_, BoxError>(HttpResponse::new(HttpBody::empty())))
        });

        let mut service = HttpMetricsLayer.layer(inner);
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
        assert_eq!(recorder.gauge_updates("rpc.connections.active", &[]), 0);
    }
}
