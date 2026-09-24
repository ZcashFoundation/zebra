//! Regression tests for bounded inventory waiting and cancellation.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::poll;
use tokio::{
    sync::{mpsc, oneshot},
    time::advance,
};
use tower::{buffer::Buffer, service_fn, util::BoxService};
use zebra_chain::{block, transaction};

use super::*;

fn single_block() -> Request {
    Request::BlocksByHash([block::Hash([0; 32])].into())
}

/// Match initialization's one-buffer stack, including its public service bounds.
fn buffered<S>(
    service: S,
) -> impl Service<
    Request,
    Response = Response,
    Error = BoxError,
    Future = BoxFuture<'static, Result<Response, BoxError>>,
> + Clone
       + Send
       + Sync
where
    S: Service<InventoryRequest, Response = Response, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
{
    retry_busy_inventory(Buffer::new(BoxService::new(service), 1))
}

fn busy(request: InventoryRequest) -> BoxError {
    InventoryBusy {
        attempted: request.attempted,
        source: None,
    }
    .into()
}

fn assert_receive_timeout(error: &BoxError) {
    assert!(error
        .downcast_ref::<SharedPeerError>()
        .expect("deadline errors retain their peer error type")
        .inner_debug()
        .contains("ConnectionReceiveTimeout"));
}

struct BecomesUnready {
    ready: bool,
    first_error: Option<BoxError>,
}

impl Service<InventoryRequest> for BecomesUnready {
    type Response = Response;
    type Error = BoxError;
    type Future = std::future::Ready<Result<Response, BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        if self.ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn call(&mut self, request: InventoryRequest) -> Self::Future {
        assert!(self.ready, "unready services must not receive requests");
        self.ready = false;
        std::future::ready(Err(self
            .first_error
            .take()
            .unwrap_or_else(|| busy(request))))
    }
}

#[tokio::test(start_paused = true)]
async fn unready_inner_service_applies_backpressure() {
    let _init_guard = zebra_test::init();
    let mut service = buffered(BecomesUnready {
        ready: false,
        first_error: None,
    });
    let request = service.ready().await.unwrap().call(single_block());
    tokio::task::yield_now().await;

    let mut other = service.clone();
    assert!(
        poll!(other.ready()).is_pending(),
        "accepted requests must consume the only buffer's capacity across clones"
    );
    drop(request);
}

#[tokio::test(start_paused = true)]
async fn initial_buffer_queue_uses_call_time_deadline() {
    let _init_guard = zebra_test::init();
    let mut service = buffered(BecomesUnready {
        ready: false,
        first_error: None,
    });
    let start = Instant::now();
    let mut response = service.ready().await.unwrap().call(single_block());

    // Do not poll the response until nearly the entire deadline has elapsed.
    advance(constants::REQUEST_TIMEOUT - Duration::from_secs(1)).await;
    assert!(poll!(response.as_mut()).is_pending());
    let error = response.await.expect_err("queued requests must expire");

    assert_receive_timeout(&error);
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn unpolled_response_cannot_extend_request_deadline() {
    let _init_guard = zebra_test::init();
    let mut service = buffered(service_fn(|_| async { Ok(Response::Nil) }));
    let response = service.ready().await.unwrap().call(single_block());
    tokio::task::yield_now().await;
    advance(constants::REQUEST_TIMEOUT).await;

    assert_receive_timeout(
        &response
            .await
            .expect_err("the call deadline already expired"),
    );
}

#[tokio::test(start_paused = true)]
async fn retry_readiness_wait_is_included_in_the_deadline() {
    let _init_guard = zebra_test::init();
    let mut service = buffered(BecomesUnready {
        ready: true,
        first_error: None,
    });
    let start = Instant::now();
    let response = service.ready().await.unwrap().call(single_block());
    // Reserve the only slot after the worker dispatches the initial request, so
    // the retry must wait in poll_ready rather than merely in the buffer queue.
    service.ready().await.unwrap();

    let error = response.await.expect_err("retry admission must be bounded");
    assert_receive_timeout(&error);
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn busy_waits_before_retrying() {
    let _init_guard = zebra_test::init();
    let mut attempts = 0;
    let service = buffered(service_fn(move |request| {
        attempts += 1;
        std::future::ready(if attempts == 1 {
            Err(busy(request))
        } else {
            Ok(Response::Nil)
        })
    }));
    let start = Instant::now();

    let response = service
        .oneshot(single_block())
        .await
        .expect("a busy peer can become available");

    assert!(matches!(response, Response::Nil));
    assert_eq!(start.elapsed(), Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn busy_expires_at_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let service = buffered(service_fn(|request| async { Err(busy(request)) }));
    let start = Instant::now();

    let error = service
        .oneshot(single_block())
        .await
        .expect_err("busy inventory waiting must be bounded");

    assert_receive_timeout(&error);
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn notfound_and_transport_errors_are_immediate() {
    let _init_guard = zebra_test::init();
    for error in [
        PeerError::NotFoundRegistry(vec![block::Hash([0; 32]).into()]),
        PeerError::ConnectionClosed,
    ] {
        let error: BoxError = SharedPeerError::from(error).into();
        let original = error.as_ref() as *const _;
        let mut returned = Some(error);
        let service = buffered(service_fn(move |_| {
            std::future::ready(Err(returned.take().expect("errors must not be retried")))
        }));
        let start = Instant::now();

        let error = service
            .oneshot(single_block())
            .await
            .expect_err("non-retry errors must reach the caller");

        assert!(std::ptr::eq(error.as_ref(), original));
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
        Request::TransactionsById(
            [transaction::UnminedTxId::Legacy(transaction::Hash([0; 32]))].into(),
        ),
    ] {
        let service = buffered(service_fn(|request| async { Err(busy(request)) }));
        let start = Instant::now();

        let error = service
            .oneshot(request)
            .await
            .expect_err("other requests must not intercept busy errors");

        assert!(error.is::<InventoryBusy>());
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn attempted_peers_survive_busy_and_rejection_retries() {
    let _init_guard = zebra_test::init();
    let peers: [PeerSocketAddr; 2] = [
        "127.0.0.1:8233".parse().unwrap(),
        "127.0.0.2:8233".parse().unwrap(),
    ];
    let mut was_busy = false;
    let service = buffered(service_fn(move |mut request: InventoryRequest| {
        let result = if request.attempted.contains(&peers[0]) && !was_busy {
            was_busy = true;
            Err(busy(request))
        } else if let Some(peer) = peers.iter().find(|peer| !request.attempted.contains(*peer)) {
            request.attempted.insert(*peer);
            Err(InventoryBusy {
                attempted: request.attempted,
                source: Some(
                    SharedPeerError::from(PeerError::NotFoundResponse(vec![
                        block::Hash([0; 32]).into()
                    ]))
                    .into(),
                ),
            }
            .into())
        } else {
            Ok(Response::Nil)
        };
        std::future::ready(result)
    }));
    let start = Instant::now();

    let response = service.oneshot(single_block()).await.unwrap();

    assert!(matches!(response, Response::Nil));
    assert_eq!(
        start.elapsed(),
        Duration::from_secs(1),
        "only busy attempts wait; explicit rejections immediately choose another peer"
    );
}

#[tokio::test(start_paused = true)]
async fn last_rejection_survives_busy_wait_at_deadline() {
    let _init_guard = zebra_test::init();
    let missing: BoxError =
        SharedPeerError::from(PeerError::NotFoundResponse(vec![
            block::Hash([0; 32]).into()
        ]))
        .into();
    let original = missing.as_ref() as *const _;
    let mut first_missing = Some(missing);
    let service = buffered(service_fn(move |request: InventoryRequest| {
        std::future::ready(Err(InventoryBusy {
            attempted: request.attempted,
            source: first_missing.take(),
        }
        .into()))
    }));
    let start = Instant::now();

    let error = service.oneshot(single_block()).await.unwrap_err();

    assert!(std::ptr::eq(error.as_ref(), original));
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn last_rejection_survives_retry_readiness_deadline() {
    let _init_guard = zebra_test::init();
    let missing: BoxError =
        SharedPeerError::from(PeerError::NotFoundResponse(vec![
            block::Hash([0; 32]).into()
        ]))
        .into();
    let original = missing.as_ref() as *const _;
    let mut service = buffered(BecomesUnready {
        ready: true,
        first_error: Some(
            InventoryBusy {
                attempted: ["127.0.0.1:8233".parse().unwrap()].into(),
                source: Some(missing),
            }
            .into(),
        ),
    });
    let start = Instant::now();
    let response = service.ready().await.unwrap().call(single_block());
    service.ready().await.unwrap();

    let error = response.await.unwrap_err();

    assert!(std::ptr::eq(error.as_ref(), original));
    assert_eq!(start.elapsed(), constants::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_the_pending_response() {
    let _init_guard = zebra_test::init();
    let (request_tx, mut request_rx) = mpsc::channel(1);
    let service = buffered(service_fn(move |_| {
        let request_tx = request_tx.clone();
        async move {
            let (response_tx, response_rx) = oneshot::channel();
            request_tx.send(response_tx).await.unwrap();
            response_rx.await.unwrap()
        }
    }));
    let mut request = Box::pin(service.oneshot(single_block()));
    let response_tx = tokio::select! {
        response_tx = request_rx.recv() => response_tx.unwrap(),
        response = request.as_mut() => panic!("unexpected response: {response:?}"),
    };

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
    let (called_tx, mut called_rx) = mpsc::unbounded_channel();
    let service = buffered(service_fn(move |request| {
        calls.fetch_add(1, Ordering::SeqCst);
        called_tx.send(()).unwrap();
        std::future::ready(Err(busy(request)))
    }));
    let mut request = Box::pin(service.oneshot(single_block()));
    tokio::select! {
        called = called_rx.recv() => called.unwrap(),
        response = request.as_mut() => panic!("unexpected response: {response:?}"),
    };
    assert!(poll!(request.as_mut()).is_pending());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    drop(request);
    advance(constants::REQUEST_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
