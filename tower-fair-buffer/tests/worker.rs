//! Fixed test cases for fair buffer worker tasks.
//!
//! Ported from `tower-batch-control`'s worker tests: the fair buffer has no
//! backpressure, so instead of pending `poll_ready` waiters, these tests
//! check that pending response futures are woken and failed on teardown.

use std::time::Duration;

use tokio_test::{assert_pending, assert_ready, assert_ready_err, task};
use tower::{Service, ServiceExt};
use tower_fair_buffer::{error, FairBuffer, Tagged};
use tower_test::mock;

/// A rotation interval long enough that tests never rotate.
const TEST_ROTATION_INTERVAL: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn wakes_pending_waiters_on_close() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) =
        FairBuffer::<_, &str, &str>::pair(service, 3, TEST_ROTATION_INTERVAL);
    let mut worker = task::spawn(worker.run());

    // keep requests in the worker and queue
    handle.allow(0);

    // The worker pops this request and parks waiting for the inner service.
    let service1 = service.ready().await.unwrap();
    let mut response1 = task::spawn(service1.call(Tagged::from_peer("peer", "req1")));
    assert_pending!(worker.poll());

    // This request stays in the queue.
    let service2 = service.ready().await.unwrap();
    let mut response2 = task::spawn(service2.call(Tagged::from_peer("peer", "req2")));
    assert_pending!(worker.poll());

    assert_pending!(response1.poll());
    assert_pending!(response2.poll());

    // kill the worker task
    drop(worker);

    assert!(
        response1.is_woken(),
        "dropping worker should wake the in-flight response task",
    );
    let err = assert_ready_err!(response1.poll());
    assert!(
        err.is::<error::Closed>(),
        "response 1 should fail with a Closed, got: {err:?}",
    );

    assert!(
        response2.is_woken(),
        "dropping worker should wake the queued response task",
    );
    let err = assert_ready_err!(response2.poll());
    assert!(
        err.is::<error::Closed>(),
        "response 2 should fail with a Closed, got: {err:?}",
    );

    // polling readiness should now fail
    let mut service1 = service.clone();
    let mut ready1 = task::spawn(service1.ready());
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready should fail with a ServiceError {{ Closed }}, got: {err:?}",
    );

    // new calls should fail immediately
    let mut response3 = task::spawn(service.call(Tagged::internal("req3")));
    let err = assert_ready_err!(response3.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "new calls should fail with a ServiceError {{ Closed }}, got: {err:?}",
    );
}

#[tokio::test]
async fn wakes_pending_waiters_on_failure() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) =
        FairBuffer::<_, &str, &str>::pair(service, 3, TEST_ROTATION_INTERVAL);
    let mut worker = task::spawn(worker.run());

    // keep requests in the worker and queue
    handle.allow(0);

    // The worker pops this request and parks waiting for the inner service.
    let service1 = service.ready().await.unwrap();
    let mut response1 = task::spawn(service1.call(Tagged::from_peer("peer", "req1")));
    assert_pending!(worker.poll());

    // This request stays in the queue.
    let service2 = service.ready().await.unwrap();
    let mut response2 = task::spawn(service2.call(Tagged::from_peer("peer", "req2")));
    assert_pending!(worker.poll());

    assert_pending!(response1.poll());
    assert_pending!(response2.poll());

    // fail the inner service
    handle.send_error("foobar");
    // worker task terminates
    assert!(
        worker.is_woken(),
        "failing the inner service should wake the worker",
    );
    assert_ready!(worker.poll());

    assert!(
        response1.is_woken(),
        "failing the inner service should wake the in-flight response task",
    );
    let err = assert_ready_err!(response1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "response 1 should fail with a ServiceError, got: {err:?}",
    );

    assert!(
        response2.is_woken(),
        "failing the inner service should wake the queued response task",
    );
    let err = assert_ready_err!(response2.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "response 2 should fail with a ServiceError, got: {err:?}",
    );

    // polling readiness should now fail
    let mut service1 = service.clone();
    let mut ready1 = task::spawn(service1.ready());
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready should fail with a ServiceError, got: {err:?}",
    );

    // new calls should fail immediately
    let mut response3 = task::spawn(service.call(Tagged::internal("req3")));
    let err = assert_ready_err!(response3.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "new calls should fail with a ServiceError, got: {err:?}",
    );
}
