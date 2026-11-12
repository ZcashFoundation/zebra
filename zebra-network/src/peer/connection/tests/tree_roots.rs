//! Tree roots request and response mapping in peer connections.

use std::time::Duration;

use futures::{channel::oneshot, SinkExt, StreamExt};
use tracing::Span;

use zebra_chain::{block, parallel::tree::TreeRoots};
use zebra_test::mock_service::PanicAssertion;

use crate::{peer::ClientRequest, protocol::external::Message, Request, Response};

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
    let roots = vec![TreeRoots::empty(); 2];

    for (reply, expected) in [
        (
            roots.clone(),
            Some(Response::TreeRoots(Some(roots.clone()))),
        ),
        (vec![TreeRoots::empty(); 3], None),
    ] {
        let (tx, rx) = oneshot::channel();
        client_tx
            .send(ClientRequest {
                request: Request::TreeRoots {
                    start_height,
                    final_hash,
                    count: 2,
                },
                tx,
                inv_collector: None,
                transient_addr: None,
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
    for roots in [Some(vec![TreeRoots::empty()]), None] {
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
