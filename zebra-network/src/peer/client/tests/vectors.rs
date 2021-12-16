//! Fixed peer [`Client`] test vectors.

use futures::{
    channel::{mpsc, oneshot},
    FutureExt,
};
use tower::ServiceExt;

use crate::{
    peer::{CancelHeartbeatTask, Client, ErrorSlot},
    protocol::external::types::Version,
    PeerError,
};

#[tokio::test]
async fn client_service_ready_ok() {
    zebra_test::init();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Ok(Client { .. }))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, None));

    let result = shutdown_rx.try_recv();
    assert!(matches!(result, Ok(None)));

    // Unlike oneshots, open futures::mpsc channels return Err when empty
    let result = server_rx.try_next();
    assert!(matches!(result, Err(_)));
}

#[tokio::test]
async fn client_service_ready_heartbeat_exit() {
    zebra_test::init();

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    shared_error_slot
        .try_update_error(PeerError::HeartbeatTaskExited.into())
        .expect("unexpected earlier error in tests");
    std::mem::drop(shutdown_rx);

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Err(_))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    // Unlike oneshots, closed futures::mpsc channels return None
    let result = server_rx.try_next();
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn client_service_ready_request_drop() {
    zebra_test::init();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (server_tx, server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    shared_error_slot
        .try_update_error(PeerError::ConnectionDropped.into())
        .expect("unexpected earlier error in tests");
    std::mem::drop(server_rx);

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Err(_))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    let result = shutdown_rx.try_recv();
    assert!(matches!(result, Ok(Some(CancelHeartbeatTask))));
}

#[tokio::test]
async fn client_service_ready_request_close() {
    zebra_test::init();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    shared_error_slot
        .try_update_error(PeerError::ConnectionClosed.into())
        .expect("unexpected earlier error in tests");
    server_rx.close();

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Err(_))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    let result = shutdown_rx.try_recv();
    assert!(matches!(result, Ok(Some(CancelHeartbeatTask))));

    let result = server_rx.try_next();
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn client_service_ready_error_in_slot() {
    zebra_test::init();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    shared_error_slot
        .try_update_error(PeerError::Overloaded.into())
        .expect("unexpected earlier error in tests");

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Err(_))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    let result = shutdown_rx.try_recv();
    assert!(matches!(result, Ok(Some(CancelHeartbeatTask))));

    let result = server_rx.try_next();
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn client_service_ready_multiple_errors() {
    zebra_test::init();

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let mut client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    shared_error_slot
        .try_update_error(PeerError::DuplicateHandshake.into())
        .expect("unexpected earlier error in tests");
    std::mem::drop(shutdown_rx);
    server_rx.close();

    let result = client.ready().now_or_never();
    assert!(matches!(result, Some(Err(_))));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    let result = server_rx.try_next();
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn client_service_drop_cleanup() {
    zebra_test::init();

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (server_tx, mut server_rx) = mpsc::channel(1);

    let shared_error_slot = ErrorSlot::default();

    let client = Client {
        shutdown_tx: Some(shutdown_tx),
        server_tx,
        error_slot: shared_error_slot.clone(),
        version: Version(0),
    };

    std::mem::drop(client);

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    let result = shutdown_rx.try_recv();
    assert!(matches!(result, Ok(Some(CancelHeartbeatTask))));

    let result = server_rx.try_next();
    assert!(matches!(result, Ok(None)));
}
