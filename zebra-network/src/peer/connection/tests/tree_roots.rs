//! Tree roots request and response mapping in peer connections.

use std::{sync::Arc, time::Duration};

use futures::{channel::oneshot, SinkExt, StreamExt};
use tracing::Span;

use zebra_chain::{
    block,
    parallel::tree::{TreeRoots, TreeRootsProof},
};
use zebra_test::mock_service::PanicAssertion;

use crate::{
    constants::REQUEST_TIMEOUT, peer::ClientRequest, protocol::external::Message, Request, Response,
};

fn root_proof() -> TreeRootsProof {
    TreeRootsProof {
        roots: TreeRoots::empty(),
        successor_auth_data_root: None,
    }
}

/// Outbound tree roots requests become `gettreeroots`, and the next `treeroots` is the response,
/// unless it has more entries than requested.
#[tokio::test(start_paused = true)]
async fn outbound_tree_roots_request_maps_to_messages() {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = futures::channel::mpsc::channel(1);
    let (connection, mut client_tx, _inbound, mut outbound, error_slot) =
        super::new_test_connection::<PanicAssertion>();
    let connection_task = tokio::spawn(connection.run(peer_rx));

    let start_height = block::Height(10);
    let final_hash = block::Hash([7; 32]);
    let roots = vec![root_proof(); 2];

    for (reply, expected) in [
        (
            roots.clone(),
            Some(Response::TreeRoots(Some(roots.clone()))),
        ),
        (vec![root_proof(); 3], None),
    ] {
        let (tx, rx) = oneshot::channel();
        client_tx
            .send(ClientRequest {
                request: Request::TreeRoots {
                    start_height,
                    final_hash,
                    count: 2,
                    excluded_peer: None,
                },
                tx,
                span: Span::current(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), outbound.next())
                .await
                .unwrap(),
            Some(Message::GetTreeRoots {
                start_height,
                final_hash,
                count: 2,
            }),
        );

        peer_tx
            .send(Ok(Message::TreeRoots(Some(reply))))
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap();
        match expected {
            Some(expected) => assert_eq!(response.unwrap(), expected),
            None => assert!(response.is_err(), "too many roots must fail the request"),
        }
    }

    assert!(error_slot.try_get_error().is_none());
    connection_task.abort();
}

/// Inbound `gettreeroots` messages reach the inbound service, and its response is sent back.
#[tokio::test(start_paused = true)]
async fn inbound_get_tree_roots_maps_to_request() {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = futures::channel::mpsc::channel(1);
    let (connection, _client_tx, mut inbound, mut outbound, error_slot) =
        super::new_test_connection::<PanicAssertion>();
    let connection_task = tokio::spawn(connection.run(peer_rx));

    let start_height = block::Height(10);
    let final_hash = block::Hash([7; 32]);
    for roots in [Some(vec![root_proof()]), None] {
        peer_tx
            .send(Ok(Message::GetTreeRoots {
                start_height,
                final_hash,
                count: 1,
            }))
            .await
            .unwrap();
        inbound
            .expect_request(Request::TreeRoots {
                start_height,
                final_hash,
                count: 1,
                excluded_peer: None,
            })
            .await
            .respond(Response::TreeRoots(roots.clone()));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), outbound.next())
                .await
                .unwrap(),
            Some(Message::TreeRoots(roots)),
        );
    }

    assert!(error_slot.try_get_error().is_none());
    connection_task.abort();
}

#[tokio::test(start_paused = true)]
async fn canceled_tree_roots_exchange_retires_connection() {
    interrupted_tree_roots_exchange_retires_connection(true).await;
}

#[tokio::test(start_paused = true)]
async fn timed_out_tree_roots_exchange_retires_connection() {
    interrupted_tree_roots_exchange_retires_connection(false).await;
}

/// A delayed identifier-free reply must not answer a later range queued on the old stream.
async fn interrupted_tree_roots_exchange_retires_connection(cancel: bool) {
    let _init_guard = zebra_test::init();
    let (mut peer_tx, peer_rx) = futures::channel::mpsc::channel(1);
    let (mut connection, mut client_tx, _inbound, mut outbound, error_slot) =
        super::new_test_connection::<PanicAssertion>();
    let permits = Arc::new(tokio::sync::Semaphore::new(1));
    connection.compact_receive_rx = tokio::sync::watch::channel(Some(permits.clone())).1;
    connection.compact_receive_permit = Some(permits.clone().try_acquire_owned().unwrap());
    let connection_task = tokio::spawn(connection.run(peer_rx));

    let request = |height, tx| ClientRequest {
        request: Request::TreeRoots {
            start_height: block::Height(height),
            final_hash: block::Hash([7; 32]),
            count: 1,
            excluded_peer: None,
        },
        tx,
        span: Span::current(),
    };
    let (tx, rx) = oneshot::channel();
    client_tx.send(request(10, tx)).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::GetTreeRoots {
            start_height: block::Height(10),
            final_hash: block::Hash([7; 32]),
            count: 1,
        }),
    );
    let (queued_tx, queued_rx) = oneshot::channel();
    client_tx.try_send(request(11, queued_tx)).unwrap();
    if cancel {
        drop(rx);
    } else {
        // Auto-advance after the connection arms its request timer, not after just its wire send.
        let error = tokio::time::timeout(REQUEST_TIMEOUT * 2, rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.inner_debug(), "ConnectionReceiveTimeout");
    }

    // Without retirement this reads the second range request, then its receiver accepts the
    // delayed empty prefix from the first exchange as a successful answer for the second range.
    let later_request = tokio::time::timeout(Duration::from_secs(1), outbound.next())
        .await
        .unwrap();
    let delayed_reply = peer_tx.send(Ok(Message::TreeRoots(Some(vec![])))).await;
    let queued_result = tokio::time::timeout(Duration::from_secs(1), queued_rx)
        .await
        .unwrap()
        .unwrap();
    let queued_error = queued_result.expect_err("an old reply must not satisfy a later root range");
    let expected_error = if cancel {
        "ConnectionDropped"
    } else {
        "ConnectionReceiveTimeout"
    };
    assert_eq!(queued_error.inner_debug(), expected_error);
    assert_eq!(
        later_request, None,
        "the old byte stream must not be reused"
    );
    assert!(
        delayed_reply.is_err(),
        "the retired transport rejects delayed replies"
    );
    tokio::time::timeout(Duration::from_secs(1), connection_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        error_slot.try_get_error().unwrap().inner_debug(),
        expected_error
    );
    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());
    assert_eq!(
        permits.available_permits(),
        1,
        "retirement releases receive slots"
    );

    // Retirement is neutral: a fresh connection to the same honest peer can serve the next range.
    let (mut peer_tx, peer_rx) = futures::channel::mpsc::channel(1);
    let (connection, mut client_tx, _inbound, mut outbound, errors) =
        super::new_test_connection::<PanicAssertion>();
    let connection_task = tokio::spawn(connection.run(peer_rx));
    let (tx, rx) = oneshot::channel();
    client_tx.send(request(11, tx)).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::GetTreeRoots {
            start_height: block::Height(11),
            final_hash: block::Hash([7; 32]),
            count: 1,
        }),
    );
    let roots = vec![root_proof()];
    peer_tx
        .send(Ok(Message::TreeRoots(Some(roots.clone()))))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Response::TreeRoots(Some(roots)),
    );
    assert!(errors.try_get_error().is_none());
    connection_task.abort();
}
