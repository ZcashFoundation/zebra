//! Fixed test cases for batch worker tasks.

// `tokio-test`'s macro expansion trips this lint, and each toolchain knows only one of its two names: https://github.com/rust-lang/rust/issues/79813
#![allow(unknown_lints)]
#![allow(semicolon_in_expressions_from_macros)]
#![allow(semicolon_in_expressions_from_non_local_macros)]

use std::time::Duration;

use tokio::time::{sleep, Instant};
use tokio_test::{assert_pending, assert_ready, assert_ready_err, task};
use tower::{Service, ServiceExt};
use tower_batch_control::{error, Batch, BatchControl};
use tower_test::mock;

#[tokio::test]
async fn wakes_pending_waiters_on_close() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) = Batch::pair(
        service,
        1,
        1,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut worker = task::spawn(worker.run());

    // // keep the request in the worker
    handle.allow(0);
    let service1 = service.ready().await.unwrap();
    let poll = worker.poll();
    assert_pending!(poll);
    let mut response = task::spawn(service1.call(()));

    let mut service1 = service.clone();
    let mut ready1 = task::spawn(service1.ready());
    assert_pending!(worker.poll());
    assert_pending!(ready1.poll(), "no capacity");

    let mut service1 = service.clone();
    let mut ready2 = task::spawn(service1.ready());
    assert_pending!(worker.poll());
    assert_pending!(ready2.poll(), "no capacity");

    // kill the worker task
    drop(worker);

    let err = assert_ready_err!(response.poll());
    assert!(
        err.is::<error::Closed>(),
        "response should fail with a Closed, got: {err:?}",
    );

    assert!(
        ready1.is_woken(),
        "dropping worker should wake ready task 1",
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 1 should fail with a ServiceError {{ Closed }}, got: {err:?}",
    );

    assert!(
        ready2.is_woken(),
        "dropping worker should wake ready task 2",
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 2 should fail with a ServiceError {{ Closed }}, got: {err:?}",
    );
}

#[tokio::test]
async fn wakes_pending_waiters_on_failure() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) = Batch::pair(
        service,
        1,
        1,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut worker = task::spawn(worker.run());

    // keep the request in the worker
    handle.allow(0);
    let service1 = service.ready().await.unwrap();
    assert_pending!(worker.poll());
    let mut response = task::spawn(service1.call("hello"));

    let mut service1 = service.clone();
    let mut ready1 = task::spawn(service1.ready());
    assert_pending!(worker.poll());
    assert_pending!(ready1.poll(), "no capacity");

    let mut service1 = service.clone();
    let mut ready2 = task::spawn(service1.ready());
    assert_pending!(worker.poll());
    assert_pending!(ready2.poll(), "no capacity");

    // fail the inner service
    handle.send_error("foobar");
    // worker task terminates
    assert_ready!(worker.poll());

    let err = assert_ready_err!(response.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "response should fail with a ServiceError, got: {err:?}"
    );

    assert!(
        ready1.is_woken(),
        "dropping worker should wake ready task 1"
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 1 should fail with a ServiceError, got: {err:?}"
    );

    assert!(
        ready2.is_woken(),
        "dropping worker should wake ready task 2"
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 2 should fail with a ServiceError, got: {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn batch_flushes_on_idle() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        64,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    let start = Instant::now();

    // Both a singleton and a burst flush on idle, including after the previous batch.
    for count in [16, 1] {
        for _ in 0..count {
            drop(service.ready().await.unwrap().call(()));
            assert_pending!(worker.poll());
            let (request, response) = assert_ready!(handle.poll_request()).unwrap();
            assert!(matches!(request, BatchControl::Item(())));
            response.send_response(());
        }

        sleep(Duration::from_millis(4)).await;
        assert_pending!(worker.poll());
        assert!(handle.poll_request().is_pending());
        sleep(Duration::from_millis(1)).await;
        assert_pending!(worker.poll());
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Flush));
        response.send_response(());
        assert_pending!(worker.poll());
        assert!(handle.poll_request().is_pending());
    }
    assert_eq!(start.elapsed(), Duration::from_millis(10));
}

#[tokio::test(start_paused = true)]
async fn batch_idle_extends_until_max_latency() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        64,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    let start = Instant::now();

    for _ in 0..25 {
        assert_pending!(worker.poll());
        assert!(handle.poll_request().is_pending());
        drop(service.ready().await.unwrap().call(()));
        assert_pending!(worker.poll());
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Item(())));
        response.send_response(());
        sleep(Duration::from_millis(4)).await;
    }

    assert_eq!(start.elapsed(), Duration::from_millis(100));
    assert_pending!(worker.poll());
    let (request, response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Flush));
    response.send_response(());
}

#[tokio::test(start_paused = true)]
async fn queued_items_extend_idle_but_not_max_latency() {
    for delay_ms in [6, 100] {
        let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
        let (mut service, worker) = Batch::pair(
            inner,
            64,
            1,
            Duration::from_millis(100),
            Duration::from_millis(5),
        );
        let mut worker = task::spawn(worker.run());
        drop(service.ready().await.unwrap().call(()));
        assert_pending!(worker.poll());
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Item(())));
        response.send_response(());

        // Input keeps arriving while the worker is not polled.
        for _ in 0..3 {
            drop(service.ready().await.unwrap().call(()));
        }
        sleep(Duration::from_millis(delay_ms)).await;
        assert_pending!(worker.poll());
        if delay_ms < 100 {
            for _ in 0..3 {
                let (request, response) = assert_ready!(handle.poll_request()).unwrap();
                assert!(matches!(request, BatchControl::Item(())));
                response.send_response(());
            }
            assert!(handle.poll_request().is_pending());
            sleep(Duration::from_millis(5)).await;
            assert_pending!(worker.poll());
        }
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Flush));
        response.send_response(());
    }
}

#[tokio::test(start_paused = true)]
async fn overflowing_batch_timeouts_flush_on_finite_bound() {
    let finite = Duration::from_millis(10);
    for (max_latency, max_idle, count, flush_after) in [
        (Duration::MAX, finite, 1, finite),
        (Duration::MAX, finite, 2, Duration::from_millis(14)),
        (finite, Duration::MAX, 1, finite),
        (finite, Duration::MAX, 2, finite),
    ] {
        let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
        let (mut service, worker) = Batch::pair(inner, 4, 1, max_latency, max_idle);
        let mut worker = task::spawn(worker.run());
        let start = Instant::now();

        // Exercise both the first-item deadline and its reset on a later item.
        for item in 0..count {
            if item > 0 {
                sleep(Duration::from_millis(4)).await;
            }
            drop(service.ready().await.unwrap().call(()));
            assert_pending!(worker.poll());
            let (request, response) = assert_ready!(handle.poll_request()).unwrap();
            assert!(matches!(request, BatchControl::Item(())));
            response.send_response(());
            assert!(handle.poll_request().is_pending());
        }

        sleep(flush_after - start.elapsed() - Duration::from_millis(1)).await;
        assert_pending!(worker.poll());
        assert!(handle.poll_request().is_pending());
        sleep(Duration::from_millis(1)).await;
        assert_pending!(worker.poll());
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Flush));
        response.send_response(());
        assert_eq!(start.elapsed(), flush_after);

        drop(service);
        assert_ready!(worker.poll());
    }
}

#[tokio::test(start_paused = true)]
async fn batch_max_items_flushes_immediately() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        4,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    let start = Instant::now();

    for _ in 0..3 {
        drop(service.ready().await.unwrap().call(()));
        assert_pending!(worker.poll());
        let (request, response) = assert_ready!(handle.poll_request()).unwrap();
        assert!(matches!(request, BatchControl::Item(())));
        response.send_response(());
        assert!(handle.poll_request().is_pending());
    }

    drop(service.ready().await.unwrap().call(()));
    assert_pending!(worker.poll());
    let (request, response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Item(())));
    response.send_response(());
    let (request, response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Flush));
    response.send_response(());
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn explicit_flush_is_ordered_and_does_not_wait_for_idle() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        64,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    let start = Instant::now();

    // Empty flushes must not start CPU work or a timer.
    assert!(service.try_flush().unwrap());
    assert_pending!(worker.poll());
    assert!(handle.poll_request().is_pending());

    let mut checking = task::spawn(service.ready().await.unwrap().call(()));
    assert!(service.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (request, item_response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Item(())));
    let (request, batch_response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Flush));
    assert_pending!(checking.poll(), "a flush does not complete verification");
    batch_response.send_response(());
    item_response.send_response(());
    assert_ready!(checking.poll()).unwrap();
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn explicit_flush_keeps_the_callers_readiness_reservation() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        64,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());

    // `call` panics if a flush between `ready` and `call` spent the reservation.
    let ready = service.ready().await.unwrap();
    assert!(ready.try_flush().unwrap());
    let mut checking = task::spawn(ready.call(()));
    assert!(service.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (request, item_response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Item(())));
    let (request, batch_response) = assert_ready!(handle.poll_request()).unwrap();
    assert!(matches!(request, BatchControl::Flush));
    batch_response.send_response(());
    item_response.send_response(());
    assert_ready!(checking.poll()).unwrap();
}

#[tokio::test(start_paused = true)]
async fn explicit_flush_preserves_queue_bounds_and_worker_cleanup() {
    let (inner, mut handle) = mock::pair::<BatchControl<()>, ()>();
    let (mut service, worker) = Batch::pair(
        inner,
        2,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    handle.allow(0);
    let mut checking = task::spawn(service.ready().await.unwrap().call(()));
    assert!(service.try_flush().unwrap());
    assert!(
        !service.try_flush().unwrap(),
        "flush markers consume queue capacity"
    );
    assert_pending!(worker.poll());
    drop(worker);
    assert_ready_err!(checking.poll());
    assert!(service.try_flush().is_err());
}

#[tokio::test(start_paused = true)]
async fn explicit_flush_releases_zero_weight_requests_without_waiting_for_idle() {
    struct ZeroWeight;

    impl tower_batch_control::RequestWeight for ZeroWeight {
        fn request_weight(&self) -> usize {
            0
        }
    }

    let (done, response) = tokio::sync::oneshot::channel();
    let mut done = Some(done);
    let mut response = Some(response);
    let inner = tower::service_fn(move |request: BatchControl<ZeroWeight>| {
        let response = match request {
            BatchControl::Item(_) => response.take(),
            BatchControl::Flush => {
                done.take().unwrap().send(()).unwrap();
                None
            }
        };
        async move {
            if let Some(response) = response {
                response.await?;
            }
            Ok::<_, tokio::sync::oneshot::error::RecvError>(())
        }
    });
    let (mut service, worker) = Batch::pair(
        inner,
        64,
        1,
        Duration::from_millis(100),
        Duration::from_millis(5),
    );
    let mut worker = task::spawn(worker.run());
    let start = Instant::now();
    let mut checking = task::spawn(service.ready().await.unwrap().call(ZeroWeight));
    assert_pending!(worker.poll());
    assert_pending!(checking.poll(), "the request waits for its batch to flush");

    assert!(service.try_flush().unwrap());
    assert_pending!(worker.poll());
    assert_ready!(checking.poll()).unwrap();
    assert_eq!(start.elapsed(), Duration::ZERO);
}
