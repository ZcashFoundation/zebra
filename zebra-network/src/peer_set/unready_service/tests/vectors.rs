//! Fixed test vectors for unready services.
//!
//! TODO: test that inner service errors are handled correctly (#3204)

use std::marker::PhantomData;

use futures::channel::oneshot;

use zebra_test::mock_service::MockService;

use crate::{
    peer_set::{
        set::CancelClientWork,
        unready_service::{Error, UnreadyService},
    },
    Request, Response, SharedPeerError,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct MockKey;

#[tokio::test]
async fn unready_service_result_ok() {
    let _init_guard = zebra_test::init();

    let (_cancel_sender, cancel) = oneshot::channel();

    let mock_client: MockService<Request, Response, _, Error<SharedPeerError>> =
        MockService::build().for_unit_tests();
    let unready_service = UnreadyService {
        key: Some(MockKey),
        cancel,
        service: Some(mock_client),
        _req: PhantomData::default(),
    };

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
        service: Some(mock_client),
        _req: PhantomData::default(),
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
        service: Some(mock_client),
        _req: PhantomData::default(),
    };

    std::mem::drop(cancel_sender);

    let result = unready_service.await;
    assert!(matches!(
        result,
        Err((MockKey, Error::CancelHandleDropped(_)))
    ));
}
