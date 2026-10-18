//! Fixed test vectors for unready services.

use std::marker::PhantomData;

use futures::{channel::oneshot, FutureExt};
use tower::{Service, ServiceExt};

use zebra_test::mock_service::MockService;

use crate::{
    constants::REQUEST_TIMEOUT,
    peer::ClientTestHarness,
    peer_set::{
        set::CancelClientWork,
        unready_service::{Error, UnreadyService},
    },
    Request, Response, SharedPeerError,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct MockKey;

#[tokio::test(start_paused = true)]
async fn unready_service_result_ok() {
    let _init_guard = zebra_test::init();

    let (_cancel_sender, cancel) = oneshot::channel();

    let mock_client: MockService<Request, Response, _, Error<SharedPeerError>> =
        MockService::build().for_unit_tests();
    let unready_service = UnreadyService {
        key: Some(MockKey),
        cancel,
        readiness_timeout: tokio::time::sleep(REQUEST_TIMEOUT),
        service: Some(mock_client),
        _req: PhantomData,
    };

    // A service that is ready when its readiness deadline expires must still be retained.
    tokio::time::advance(REQUEST_TIMEOUT).await;

    let result = unready_service.await;
    assert!(matches!(result, Ok((MockKey, MockService { .. }))));
}

#[tokio::test]
async fn unready_service_result_canceled() {
    let _init_guard = zebra_test::init();

    let (cancel_sender, cancel) = oneshot::channel();

    let mock_client: MockService<Request, Response, _, Error<SharedPeerError>> =
        MockService::build().for_unit_tests();
    let unready_service = UnreadyService {
        key: Some(MockKey),
        cancel,
        readiness_timeout: tokio::time::sleep(REQUEST_TIMEOUT),
        service: Some(mock_client),
        _req: PhantomData,
    };

    cancel_sender
        .send(CancelClientWork)
        .expect("unexpected oneshot send failure in tests");

    let result = unready_service.await;
    assert!(matches!(result, Err((MockKey, Error::Canceled))));
}

#[tokio::test]
async fn unready_service_result_cancel_handle_dropped() {
    let _init_guard = zebra_test::init();

    let (cancel_sender, cancel) = oneshot::channel();

    let mock_client: MockService<Request, Response, _, Error<SharedPeerError>> =
        MockService::build().for_unit_tests();
    let unready_service = UnreadyService {
        key: Some(MockKey),
        cancel,
        readiness_timeout: tokio::time::sleep(REQUEST_TIMEOUT),
        service: Some(mock_client),
        _req: PhantomData,
    };

    std::mem::drop(cancel_sender);

    let result = unready_service.await;
    assert!(matches!(
        result,
        Err((MockKey, Error::CancelHandleDropped(_)))
    ));
}

/// The bound starts at insertion, survives repeated polls, and drops the client on timeout.
#[tokio::test(start_paused = true)]
async fn unready_service_times_out_from_insertion() {
    let _init_guard = zebra_test::init();
    let (_cancel_sender, cancel) = oneshot::channel();
    let (mut client, mut harness) = ClientTestHarness::build().finish();

    // Fill both slots in the harness's channel(1), including the sender's reserved slot.
    let _requests = [
        client.ready().await.unwrap().call(Request::Peers),
        client.ready().await.unwrap().call(Request::Peers),
    ];
    assert!(client.ready().now_or_never().is_none());

    let mut unready_service = Box::pin(UnreadyService {
        key: Some(MockKey),
        cancel,
        readiness_timeout: tokio::time::sleep(REQUEST_TIMEOUT),
        service: Some(client),
        _req: PhantomData,
    });

    // Delay the first poll, then keep polling without restarting the readiness budget.
    for _ in 0..3 {
        tokio::time::advance(REQUEST_TIMEOUT / 4).await;
        assert!(unready_service.as_mut().now_or_never().is_none());
        assert!(harness.wants_connection_heartbeats());
    }

    tokio::time::advance(REQUEST_TIMEOUT / 4).await;
    assert!(matches!(
        unready_service.as_mut().now_or_never(),
        Some(Err((MockKey, Error::TimedOut)))
    ));

    // Like cancellation, timing out leaves shutdown to the future owner's drop.
    drop(unready_service);
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.current_error().is_some());
}
