//! Randomised property tests for peer connection handling.

use std::{collections::HashSet, env, mem, sync::Arc};

use futures::{
    channel::{mpsc, oneshot},
    sink::SinkMapErr,
    SinkExt, StreamExt,
};
use proptest::{collection, prelude::*};
use tracing::Span;

use zebra_chain::{
    block::{self, Block},
    fmt::DisplayToDebug,
    serialization::SerializationError,
};
use zebra_test::mock_service::{MockService, PropTestAssertion};

use crate::{
    constants::{MAX_ADDRS_IN_MESSAGE, PEER_ADDR_RESPONSE_LIMIT},
    meta_addr::MetaAddr,
    peer::{
        connection::{Connection, Handler},
        ClientRequest, ErrorSlot,
    },
    protocol::external::Message,
    protocol::internal::InventoryResponse,
    Request, Response, SharedPeerError,
};

use InventoryResponse::*;

proptest! {
    // The default value of proptest cases (256) causes this test to take more than ten seconds on
    // most machines, so this reduces the value a little to reduce the test time.
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32))
    )]

    /// This test makes sure that Zebra ignores extra blocks after a block request is cancelled.
    ///
    /// We need this behaviour to avoid cascading errors after a single cancelled block request.
    #[test]
    fn connection_is_not_desynchronized_when_request_is_cancelled(
        first_block in any::<DisplayToDebug<Arc<Block>>>(),
        second_block in any::<DisplayToDebug<Arc<Block>>>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            // The real stream and sink are from a split TCP connection,
            // but that doesn't change how the state machine behaves.
            let (mut peer_tx, peer_rx) = mpsc::channel(1);

            let (
                connection,
                mut client_tx,
                mut inbound_service,
                mut peer_outbound_messages,
                shared_error_slot,
            ) = new_test_connection();

            let connection_task = tokio::spawn(connection.run(peer_rx));

            let response_to_first_request = send_block_request(
                first_block.hash(),
                &mut client_tx,
                &mut peer_outbound_messages,
            )
            .await;

            // Cancel first request.
            mem::drop(response_to_first_request);

            let response_to_second_request = send_block_request(
                second_block.hash(),
                &mut client_tx,
                &mut peer_outbound_messages,
            )
            .await;

            // Reply to first request
            peer_tx
                .send(Ok(Message::Block(first_block.0)))
                .await
                .expect("Failed to send response to first block request");

            // Reply to second request
            peer_tx
                .send(Ok(Message::Block(second_block.0.clone())))
                .await
                .expect("Failed to send response to second block request");

            // Check second response is correctly received
            let receive_response_result = response_to_second_request.await;

            prop_assert!(
                receive_response_result.is_ok(),
                "unexpected receive result: {:?}",
                receive_response_result,
            );
            let response_result = receive_response_result.unwrap();

            prop_assert!(
                response_result.is_ok(),
                "unexpected response result: {:?}",
                response_result,
            );
            let response = response_result.unwrap();

            prop_assert_eq!(response, Response::Blocks(vec![Available((second_block.0, None))]));

            // Check the state after the response
            let error = shared_error_slot.try_get_error();
            assert!(error.is_none());

            inbound_service.expect_no_requests().await?;

            // Stop the connection thread
            mem::drop(peer_tx);

            let connection_task_result = connection_task.await;
            prop_assert!(
                connection_task_result.is_ok(),
                "unexpected task result: {:?}",
                connection_task_result,
            );

            Ok(())
        })?;
    }

    /// This test makes sure that Zebra's per-connection peer cache is updated correctly.
    #[test]
    fn cache_is_updated_correctly(
        mut cached_addrs in collection::vec(MetaAddr::gossiped_strategy(), 0..=MAX_ADDRS_IN_MESSAGE),
        new_addrs in collection::vec(MetaAddr::gossiped_strategy(), 0..=MAX_ADDRS_IN_MESSAGE),
        response_size in 0..=PEER_ADDR_RESPONSE_LIMIT,
    ) {
        let _init_guard = zebra_test::init();

        let old_cached_addrs = cached_addrs.clone();

        let response = Handler::update_addr_cache(&mut cached_addrs, &new_addrs, response_size);

        prop_assert!(cached_addrs.len() <= MAX_ADDRS_IN_MESSAGE, "cache has a limited size");
        prop_assert!(response.len() <= response_size, "response has a limited size");

        prop_assert!(response.len() <= old_cached_addrs.len() + new_addrs.len(), "no duplicate or made up addresses in response");
        prop_assert!(cached_addrs.len() <= old_cached_addrs.len() + new_addrs.len(), "no duplicate or made up addresses in cache");

        if old_cached_addrs.len() + new_addrs.len() >= response_size {
            // If we deduplicate addresses, this check should fail and must be changed
            prop_assert_eq!(response.len(), response_size, "response gets addresses before cache does");
        } else {
            prop_assert!(response.len() < response_size, "response gets all addresses if there is no excess");
        }

        if old_cached_addrs.len() + new_addrs.len() <= response_size {
            prop_assert_eq!(cached_addrs.len(), 0, "cache is empty if there are no excess addresses");
        } else {
            // If we deduplicate addresses, this check should fail and must be changed
            prop_assert_ne!(cached_addrs.len(), 0, "cache gets excess addresses");
        }

    }
}

/// Creates a new [`Connection`] instance for property tests.
fn new_test_connection() -> (
    Connection<
        MockService<Request, Response, PropTestAssertion>,
        SinkMapErr<mpsc::Sender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, PropTestAssertion>,
    mpsc::Receiver<Message>,
    ErrorSlot,
) {
    super::new_test_connection()
}

async fn send_block_request(
    block: block::Hash,
    client_requests: &mut mpsc::Sender<ClientRequest>,
    outbound_messages: &mut mpsc::Receiver<Message>,
) -> oneshot::Receiver<Result<Response, SharedPeerError>> {
    let (response_sender, response_receiver) = oneshot::channel();

    let request = Request::BlocksByHash(HashSet::from_iter([block]));
    let client_request = ClientRequest {
        request,
        tx: response_sender,
        // we skip inventory collection in these tests
        inv_collector: None,
        transient_addr: None,
        span: Span::none(),
    };

    client_requests
        .send(client_request)
        .await
        .expect("failed to send block request to connection task");

    let request_message = outbound_messages
        .next()
        .await
        .expect("First block request message not sent");

    assert_eq!(request_message, Message::GetData(vec![block.into()]));

    response_receiver
}
