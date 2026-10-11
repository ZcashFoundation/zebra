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
