//! Regression tests for bounded inventory waiting and cancellation.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::poll;
use tokio::{
    sync::{mpsc, oneshot},
    time::{advance, Instant},
};
use tower::service_fn;
use zebra_chain::block;

use super::*;

fn single_block() -> Request {
    Request::BlocksByHash([block::Hash([0; 32])].into())
}

#[derive(Clone)]
struct BecomesUnready {
    ready: bool,
}

impl Service<Request> for BecomesUnready {
    type Response = Response;
    type Error = BoxError;
    type Future = std::future::Ready<Result<Response, BoxError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), BoxError>> {
        if self.ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn call(&mut self, _request: Request) -> Self::Future {
        assert!(self.ready, "unready services must not receive requests");
        self.ready = false;
        std::future::ready(Err(InventoryBusy.into()))
    }
}

#[tokio::test]
async fn unready_inner_service_applies_backpressure() {
    let _init_guard = zebra_test::init();
    let mut service = retry_busy_inventory(BecomesUnready { ready: false });
    assert!(
        poll!(service.ready()).is_pending(),
        "callers must wait for inner buffer capacity before submitting a request"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_readiness_wait_is_included_in_the_deadline() {
    let _init_guard = zebra_test::init();
    let start = Instant::now();

    let error = retry_busy_inventory(BecomesUnready { ready: true })
        .oneshot(single_block())
        .await
        .expect_err("readiness waiting must be bounded");

    assert!(error.is::<SharedPeerError>());
    assert_eq!(
        error.to_string(),
        PeerError::ConnectionReceiveTimeout.to_string()
    );
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn busy_waits_before_retrying() {
    let _init_guard = zebra_test::init();
    let mut attempts = 0;
    let service = service_fn(move |_| {
        attempts += 1;
        let result = if attempts == 1 {
            Err(InventoryBusy.into())
        } else {
            Ok(Response::Nil)
        };
        std::future::ready(result)
    });
    let start = Instant::now();

    let response = retry_busy_inventory(service)
        .oneshot(single_block())
        .await
        .expect("a busy peer can become available");

    assert!(matches!(response, Response::Nil));
    assert_eq!(start.elapsed(), Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn busy_expires_at_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let service = service_fn(|_| async { Err::<Response, BoxError>(InventoryBusy.into()) });
    let start = Instant::now();

    let error = retry_busy_inventory(service)
        .oneshot(single_block())
        .await
        .expect_err("busy inventory waiting must be bounded");

    assert!(error.is::<SharedPeerError>());
    assert_eq!(
        error.to_string(),
        PeerError::ConnectionReceiveTimeout.to_string()
    );
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn notfound_and_transport_errors_are_immediate() {
    let _init_guard = zebra_test::init();
    for error in [
        PeerError::NotFoundRegistry(vec![block::Hash([0; 32]).into()]),
        PeerError::ConnectionClosed,
    ] {
        let expected = SharedPeerError::from(error);
        let returned = expected.clone();
        let service = service_fn(move |_| {
            std::future::ready(Err::<Response, BoxError>(returned.clone().into()))
        });
        let start = Instant::now();

        let error = retry_busy_inventory(service)
            .oneshot(single_block())
            .await
            .expect_err("non-busy errors must reach the caller");

        assert!(error.is::<SharedPeerError>());
        assert_eq!(error.to_string(), expected.to_string());
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn only_single_block_requests_retry_busy() {
    let _init_guard = zebra_test::init();
    for request in [
        Request::Peers,
        Request::BlocksByHash(Default::default()),
        Request::BlocksByHash([block::Hash([0; 32]), block::Hash([1; 32])].into()),
    ] {
        let service = service_fn(|_| async { Err::<Response, BoxError>(InventoryBusy.into()) });
        let start = Instant::now();

        let error = retry_busy_inventory(service)
            .oneshot(request)
            .await
            .expect_err("other requests must not intercept busy errors");

        assert!(error.is::<InventoryBusy>());
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_the_pending_response() {
    let _init_guard = zebra_test::init();
    let (request_tx, mut request_rx) = mpsc::channel(1);
    let service = service_fn(move |_| {
        let request_tx = request_tx.clone();
        async move {
            let (response_tx, response_rx) = oneshot::channel();
            request_tx.send(response_tx).await.unwrap();
            response_rx.await.unwrap()
        }
    });
    let mut request = Box::pin(retry_busy_inventory(service).oneshot(single_block()));
    assert!(poll!(request.as_mut()).is_pending());
    let response_tx = request_rx.try_recv().unwrap();

    drop(request);

    assert!(
        response_tx.is_closed(),
        "cancellation must reach the inner request"
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_stops_busy_retries() {
    let _init_guard = zebra_test::init();
    let attempts = Arc::new(AtomicUsize::new(0));
    let calls = attempts.clone();
    let service = service_fn(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Err::<Response, BoxError>(InventoryBusy.into()))
    });
    let mut request = Box::pin(retry_busy_inventory(service).oneshot(single_block()));
    assert!(poll!(request.as_mut()).is_pending());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    drop(request);
    advance(constants::REQUEST_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
