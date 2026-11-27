//! Tests for RPC metric method labels, active-request tracking, and accumulation.

use std::sync::Arc;

use futures::future;
use jsonrpsee::{
    server::middleware::rpc::RpcServiceT,
    types::{Id, Request},
    MethodResponse, ResponsePayload, RpcModule,
};
use tokio::sync::Notify;

use super::RpcMetricsMiddleware;
use crate::metrics::tests::TestMetricsRecorder;

#[derive(Clone)]
struct OkService;

impl<'a> RpcServiceT<'a> for OkService {
    type Future = std::future::Ready<MethodResponse>;

    fn call(&self, request: Request<'a>) -> Self::Future {
        std::future::ready(MethodResponse::response(
            request.id.into_owned(),
            ResponsePayload::success_borrowed(&"ok"),
            usize::MAX,
        ))
    }
}

#[derive(Clone)]
struct PendingService {
    started: Arc<Notify>,
}

impl<'a> RpcServiceT<'a> for PendingService {
    type Future = futures::future::BoxFuture<'a, MethodResponse>;

    fn call(&self, _request: Request<'a>) -> Self::Future {
        let started = self.started.clone();
        Box::pin(async move {
            started.notify_one();
            future::pending().await
        })
    }
}

#[test]
fn method_labels_use_registered_names_or_unknown() {
    let mut rpc = RpcModule::new(());
    rpc.register_method("getinfo", |_, _, _| "ok")
        .expect("getinfo is the first method registered in this module");
    let middleware = RpcMetricsMiddleware::new((), Arc::new(rpc.method_names().collect()));

    assert_eq!(middleware.method_label("getinfo"), "getinfo");
    assert_eq!(middleware.method_label("unregistered_method"), "unknown");
}

#[tokio::test]
async fn active_requests_decrements_on_completion_and_drop() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let methods = Arc::new(["getinfo"].into_iter().collect());

    let middleware = RpcMetricsMiddleware::new(OkService, Arc::clone(&methods));
    let response = middleware
        .call(Request::new("getinfo".into(), None, Id::Number(1)))
        .await;
    assert!(response.is_success());
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(0.0));

    let started = Arc::new(Notify::new());
    let pending_middleware = RpcMetricsMiddleware::new(
        PendingService {
            started: started.clone(),
        },
        methods,
    );
    let fut = pending_middleware.call(Request::new("getinfo".into(), None, Id::Number(2)));
    let handle = tokio::spawn(fut);

    started.notified().await;
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(1.0));

    handle.abort();
    let _ = handle.await;
    assert_eq!(recorder.gauge("rpc.active_requests", &[]), Some(0.0));
}

#[tokio::test]
async fn per_request_middleware_instances_accumulate_metrics() {
    let _init_guard = zebra_test::init();
    let recorder = TestMetricsRecorder::default();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let methods = Arc::new(["getinfo"].into_iter().collect());
    for id in 1..=3 {
        let middleware = RpcMetricsMiddleware::new(OkService, Arc::clone(&methods));
        let response = middleware
            .call(Request::new("getinfo".into(), None, Id::Number(id)))
            .await;
        assert!(response.is_success());
    }

    assert_eq!(
        recorder.counter(
            "rpc.requests.total",
            &[("method", "getinfo"), ("status", "success")],
        ),
        Some(3)
    );
    assert_eq!(
        recorder
            .histogram("rpc.request.duration_seconds", &[("method", "getinfo")])
            .len(),
        3
    );
}
