//! Fixed test cases for batch worker tasks.

use std::time::Duration;

use tokio_test::{assert_pending, assert_ready, assert_ready_err, task};
use tower::{Service, ServiceExt};
use tower_batch::{error, Batch};
use tower_test::mock;

#[tokio::test]
async fn wakes_pending_waiters_on_close() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) = Batch::pair(service, 1, 1, Duration::from_secs(1));
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
        "response should fail with a Closed, got: {:?}",
        err,
    );

    assert!(
        ready1.is_woken(),
        "dropping worker should wake ready task 1",
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 1 should fail with a ServiceError {{ Closed }}, got: {:?}",
        err,
    );

    assert!(
        ready2.is_woken(),
        "dropping worker should wake ready task 2",
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 2 should fail with a ServiceError {{ Closed }}, got: {:?}",
        err,
    );
}

#[tokio::test]
async fn wakes_pending_waiters_on_failure() {
    let _init_guard = zebra_test::init();

    let (service, mut handle) = mock::pair::<_, ()>();

    let (mut service, worker) = Batch::pair(service, 1, 1, Duration::from_secs(1));
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
        "response should fail with a ServiceError, got: {:?}",
        err
    );

    assert!(
        ready1.is_woken(),
        "dropping worker should wake ready task 1"
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 1 should fail with a ServiceError, got: {:?}",
        err
    );

    assert!(
        ready2.is_woken(),
        "dropping worker should wake ready task 2"
    );
    let err = assert_ready_err!(ready1.poll());
    assert!(
        err.is::<error::ServiceError>(),
        "ready 2 should fail with a ServiceError, got: {:?}",
        err
    );
}
