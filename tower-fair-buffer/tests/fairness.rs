//! Fixed test cases for fair buffer prioritization, shedding, and decay.
//!
//! These tests drive the worker task manually, so dispatch and shed order
//! are fully deterministic. The inner mock service receives requests at
//! dispatch time, so `handle.poll_request()` observes the dispatch order.

use std::time::Duration;

use tokio_test::{assert_pending, assert_ready, assert_ready_err, assert_ready_ok, task};
use tower::{Service, ServiceExt};
use tower_fair_buffer::{error, FairBuffer, Tagged};
use tower_test::mock::{self, Handle, Mock};

/// A rotation interval long enough that only [`rotation_decay`] rotates.
const TEST_ROTATION_INTERVAL: Duration = Duration::from_secs(60);

/// A test fair buffer over a string mock service, keyed by string peer names.
type TestBuffer = FairBuffer<Mock<&'static str, &'static str>, &'static str, &'static str>;

/// Returns a fair buffer with the given `capacity`, and its mock handle.
///
/// The worker is returned unspawned, so tests control exactly when it runs.
fn test_buffer(
    capacity: usize,
) -> (
    TestBuffer,
    impl std::future::Future<Output = ()>,
    Handle<&'static str, &'static str>,
) {
    let (service, handle) = mock::pair();
    let (buffer, worker) = FairBuffer::pair(service, capacity, TEST_ROTATION_INTERVAL);

    (buffer, worker.run(), handle)
}

/// Calls `service` with a request from `key`, returning its spawned response
/// future.
async fn send(
    service: &mut TestBuffer,
    key: Option<&'static str>,
    request: &'static str,
) -> task::Spawn<<TestBuffer as Service<Tagged<&'static str, &'static str>>>::Future> {
    let service = service.ready().await.expect("fair buffer is always ready");
    task::spawn(service.call(Tagged { key, request }))
}

/// Asserts that the mock handle receives exactly `expected` requests, in
/// order, and responds to each by echoing it.
fn assert_dispatch_order(
    handle: &mut Handle<&'static str, &'static str>,
    expected: &[&'static str],
) {
    for expected_request in expected {
        let (request, send_response) = assert_ready!(handle.poll_request())
            .expect("mock service is open while the fair buffer is running");
        assert_eq!(
            request, *expected_request,
            "requests should be dispatched in priority order",
        );
        send_response.send_response(request);
    }

    assert_pending!(
        handle.poll_request(),
        "no further requests should be dispatched",
    );
}

/// The compile-time service contract required by Zebra's network stack.
#[test]
fn fair_buffer_is_clone_send_sync() {
    fn assert_clone_send_sync<T: Clone + Send + Sync + 'static>() {}

    assert_clone_send_sync::<TestBuffer>();
}

/// Requests are dispatched quietest caller first, FIFO within equal counts.
#[tokio::test]
async fn quiet_peer_served_first() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(10);
    let mut worker = task::spawn(worker);

    // Queue all requests before the worker runs: a loud peer sends three
    // requests, then a quiet peer sends one.
    handle.allow(0);
    let _responses = [
        send(&mut service, Some("loud"), "loud1").await,
        send(&mut service, Some("loud"), "loud2").await,
        send(&mut service, Some("loud"), "loud3").await,
        send(&mut service, Some("quiet"), "quiet1").await,
    ];

    // The quiet peer's request beats the loud peer's second and third
    // requests, but not its first: both peers had a count of 1 when those
    // were sent, and FIFO order breaks the tie.
    handle.allow(4);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["loud1", "quiet1", "loud2", "loud3"]);
}

/// When the buffer is full, the queued request with the highest recent
/// request count is shed, and a request that is itself the highest sheds
/// itself.
#[tokio::test]
async fn shed_highest_when_full() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(2);
    let mut worker = task::spawn(worker);

    // Fill the buffer with a loud peer's requests.
    handle.allow(0);
    let mut loud1 = send(&mut service, Some("loud"), "loud1").await;
    let mut loud2 = send(&mut service, Some("loud"), "loud2").await;

    // A quiet peer's request sheds the loud peer's highest-count request.
    let mut quiet1 = send(&mut service, Some("quiet"), "quiet1").await;
    let err = assert_ready_err!(loud2.poll());
    assert!(
        err.is::<error::Shed>(),
        "the loud peer's queued request should be shed, got: {err:?}",
    );
    assert_pending!(quiet1.poll());

    // A request that is itself the highest count sheds itself, leaving the
    // queue unchanged.
    let mut loud3 = send(&mut service, Some("loud"), "loud3").await;
    let err = assert_ready_err!(loud3.poll());
    assert!(
        err.is::<error::Shed>(),
        "the loud peer's incoming request should shed itself, got: {err:?}",
    );

    // The surviving requests are dispatched in priority order and complete.
    handle.allow(2);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["loud1", "quiet1"]);

    assert_eq!(assert_ready_ok!(loud1.poll()), "loud1");
    assert_eq!(assert_ready_ok!(quiet1.poll()), "quiet1");
}

/// Internal requests always have priority 0: they are dispatched before
/// peer requests that were queued earlier.
#[tokio::test]
async fn internal_dispatches_first() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(10);
    let mut worker = task::spawn(worker);

    // Two peer requests are queued before the internal request.
    handle.allow(0);
    let _responses = [
        send(&mut service, Some("peer"), "peer1").await,
        send(&mut service, Some("peer"), "peer2").await,
        send(&mut service, None, "internal1").await,
    ];

    handle.allow(3);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["internal1", "peer1", "peer2"]);
}

/// Internal requests are never shed, even when they overflow the buffer's
/// capacity, and a full buffer of internal requests sheds incoming peer
/// requests instead.
#[tokio::test]
async fn internal_never_shed() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(2);
    let mut worker = task::spawn(worker);

    // Overfill the buffer with internal requests: none are shed.
    handle.allow(0);
    let mut internals = [
        send(&mut service, None, "internal1").await,
        send(&mut service, None, "internal2").await,
        send(&mut service, None, "internal3").await,
    ];
    for internal in &mut internals {
        assert_pending!(internal.poll());
    }

    // A peer request can't displace internal requests, so it sheds itself.
    let mut peer1 = send(&mut service, Some("peer"), "peer1").await;
    let err = assert_ready_err!(peer1.poll());
    assert!(
        err.is::<error::Shed>(),
        "peer requests should be shed while the buffer is full of internal requests, \
         got: {err:?}",
    );

    // All the internal requests complete.
    handle.allow(3);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["internal1", "internal2", "internal3"]);

    for (internal, expected) in internals
        .iter_mut()
        .zip(["internal1", "internal2", "internal3"])
    {
        assert_eq!(assert_ready_ok!(internal.poll()), expected);
    }
}

/// Recent request counts decay after two rotation intervals: an old loud
/// peer is prioritized like a fresh peer.
#[tokio::test(start_paused = true)]
async fn rotation_decay() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(10);
    let mut worker = task::spawn(worker);

    // The loud peer builds up a request count of 3.
    handle.allow(3);
    let mut responses = [
        send(&mut service, Some("loud"), "loud1").await,
        send(&mut service, Some("loud"), "loud2").await,
        send(&mut service, Some("loud"), "loud3").await,
    ];
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["loud1", "loud2", "loud3"]);
    for (response, expected) in responses.iter_mut().zip(["loud1", "loud2", "loud3"]) {
        assert_eq!(assert_ready_ok!(response.poll()), expected);
    }

    // Advance time past two rotations, polling the worker so its rotation
    // timer fires: the loud peer's count fully expires.
    tokio::time::advance(TEST_ROTATION_INTERVAL + Duration::from_secs(1)).await;
    assert!(worker.is_woken(), "rotation timer should wake the worker");
    assert_pending!(worker.poll());
    tokio::time::advance(TEST_ROTATION_INTERVAL + Duration::from_secs(1)).await;
    assert!(worker.is_woken(), "rotation timer should wake the worker");
    assert_pending!(worker.poll());

    // A mid peer sends two requests, then the loud peer sends one. Without
    // decay, the loud peer's request would have a count of 4 and sort last;
    // with decay it has a count of 1, beating the mid peer's second request.
    handle.allow(0);
    let _responses = [
        send(&mut service, Some("mid"), "mid1").await,
        send(&mut service, Some("mid"), "mid2").await,
        send(&mut service, Some("loud"), "loud4").await,
    ];

    handle.allow(3);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["mid1", "loud4", "mid2"]);
}

/// Requests whose response futures were dropped are skipped, not dispatched.
#[tokio::test]
async fn canceled_request_skipped() {
    let _init_guard = zebra_test::init();

    let (mut service, worker, mut handle) = test_buffer(10);
    let mut worker = task::spawn(worker);

    handle.allow(0);
    let canceled = send(&mut service, Some("peer"), "canceled").await;
    let _kept = send(&mut service, Some("peer"), "kept").await;

    // The caller gives up on the first request before it is dispatched.
    drop(canceled);

    // The worker skips the canceled request: the inner service never sees it.
    handle.allow(2);
    assert_pending!(worker.poll());
    assert_dispatch_order(&mut handle, &["kept"]);
}
