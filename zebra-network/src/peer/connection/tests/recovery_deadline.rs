//! Compact recovery keeps its deadline while unrelated work waits for the shared transport.

use std::{sync::Arc, time::Duration};

use futures::{channel::oneshot, FutureExt, Sink, StreamExt};
use zebra_chain::{
    block::Block,
    serialization::{SerializationError, ZcashDeserializeInto},
};
use zebra_test::mock_service::PanicAssertion;

use super::{new_test_connection, new_test_connection_with_sink};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock},
    peer::{
        connection::{compact_relay::response_timeout, Handler, State},
        Connection, MustUseClientResponseSender,
    },
    peer_set::BlockAdvert,
    protocol::external::{types::Nonce, InventoryHash, Message},
    InventoryResponse, Request, Response, SharedPeerError,
};

/// Start at the point where the missing-transaction request has finished its local write.
fn recovering<S, Tx, C>(
    connection: &mut Connection<S, Tx, C>,
    block: &Block,
) -> oneshot::Receiver<Result<Response, SharedPeerError>>
where
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    let compact = Arc::new(CompactBlock::from_block(block, 7).unwrap());
    let reconstruction = compact.reconstruct(Vec::new()).unwrap();
    assert!(!reconstruction.missing_indexes().is_empty());
    let handler = Handler::CompactBlock {
        reconstruction,
        permit: Arc::new(tokio::sync::Semaphore::new(1))
            .try_acquire_owned()
            .unwrap(),
    };
    let (tx, rx) = oneshot::channel();
    connection.request_timer = Some(Box::pin(tokio::time::sleep(response_timeout(&handler))));
    connection.state = State::AwaitingResponse {
        handler,
        tx: MustUseClientResponseSender::new(
            tx,
            &Request::BlocksByHash([block.hash()].into()),
            None,
            None,
        ),
        span: tracing::Span::none(),
    };
    rx
}

#[tokio::test(start_paused = true)]
async fn compact_recovery_deadline_defers_advertisements() {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .as_slice()
        .zcash_deserialize_into()
        .unwrap();
    let (mut connection, _client, mut inbound, mut outbound, errors) =
        new_test_connection::<PanicAssertion>();
    Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
    connection.compact.send = true;
    let (advert_tx, advert_rx) = tokio::sync::mpsc::channel(1);
    connection.block_advert_rx = advert_rx;
    let response = recovering(&mut connection, &block);
    let (mut peer, peer_rx) = futures::channel::mpsc::channel(1);
    let run = connection.run(peer_rx);
    tokio::pin!(run);
    assert!(futures::poll!(&mut run).is_pending());
    assert!(matches!(
        outbound.try_recv(),
        Ok(Message::SendCompact { .. })
    ));
    assert!(futures::poll!(&mut run).is_pending());

    let hash = zebra_chain::block::Hash([1; 32]);
    let (result, delivered) = tokio::sync::oneshot::channel();
    advert_tx
        .try_send(BlockAdvert {
            hash,
            compact_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            result,
        })
        .unwrap();
    // A dequeued advert would wait on the unanswered inbound service for twenty seconds.
    assert!(futures::poll!(&mut run).is_pending());
    tokio::time::advance(Duration::from_millis(1999)).await;
    assert!(futures::poll!(&mut run).is_pending());
    assert!(outbound.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(2)).await;
    assert!(futures::poll!(&mut run).is_pending());
    assert_eq!(
        outbound.try_recv().unwrap(),
        Message::GetData(vec![block.hash().into()])
    );

    // Finish the fallback write, then require the queued advert to resume without advancing time.
    assert!(futures::poll!(&mut run).is_pending());
    inbound
        .expect_request(Request::EarlyBlocksByHash([hash].into()))
        .now_or_never()
        .expect("the deferred advert resumes immediately after fallback")
        .respond(Response::Blocks(vec![InventoryResponse::Missing(hash)]));
    assert!(futures::poll!(&mut run).is_pending());
    assert_eq!(
        outbound.try_recv().unwrap(),
        Message::Inv(vec![hash.into()])
    );
    assert!(futures::poll!(&mut run).is_pending());
    delivered.now_or_never().unwrap().unwrap().unwrap();
    peer.try_send(Ok(Message::Block(block.clone()))).unwrap();
    assert!(futures::poll!(&mut run).is_pending());
    assert_eq!(
        response.now_or_never().unwrap().unwrap().unwrap(),
        Response::EarlyBlocks(vec![InventoryResponse::Available((block, None))])
    );
    assert!(errors.try_get_error().is_none());
}

#[tokio::test]
async fn compact_recovery_deadline_defers_backpressured_serving() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let (started, mut writes) = tokio::sync::mpsc::channel(4);
        let (release, flushed) = oneshot::channel();
        let sink = Box::pin(futures::sink::unfold(
            Some(flushed),
            move |mut flushed, message| {
                let recovery = matches!(message, Message::BlockTransactions(_));
                started.try_send(message).unwrap();
                async move {
                    if recovery {
                        flushed.take().unwrap().await.unwrap();
                    }
                    Ok::<_, SerializationError>(flushed)
                }
            },
        ));
        let (mut connection, _client, mut inbound, errors) =
            new_test_connection_with_sink::<PanicAssertion, _>(sink);
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let (advert, ()) = tokio::join!(connection.compact_advert(block.hash()), async {
            inbound
                .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                .await
                .respond(Response::Blocks(vec![InventoryResponse::Available((
                    block.clone(),
                    None,
                ))]));
        });
        assert!(advert.is_some());
        connection.compact.mark_sent();
        let response = recovering(&mut connection, &block);
        let (mut peer, peer_rx) = futures::channel::mpsc::channel(1);
        let run = connection.run(peer_rx);
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        assert!(matches!(writes.try_recv(), Ok(Message::SendCompact { .. })));

        peer.try_send(Ok(Message::GetBlockTransactions(
            BlockTransactionsRequest {
                block_hash: block.hash(),
                indexes: vec![0],
            },
        )))
        .unwrap();
        peer.try_send(Ok(Message::Ping(Nonce(9)))).unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        peer.try_send(Ok(Message::BlockTransactions(BlockTransactions {
            block_hash: block.hash(),
            transactions: block.transactions[1..].to_vec(),
        })))
        .unwrap();
        // The reply was queued at t0 behind two unrelated messages. Neither service backpressure
        // nor the deferred framed write can force the two-second full-block fallback.
        let completed = tokio::select! {
            _ = &mut run => panic!("recovery must keep the connection open"),
            completed = async {
                inbound
                    .expect_request_that(|request| {
                        matches!(request, Request::CheckCompactBlock(checked, Some(evidence))
                            if **checked == *block && evidence.evidence_for(checked).is_some())
                    })
                    .await
                    .respond(Response::Nil);
                response.await.unwrap().unwrap()
            } => completed,
        };
        let Response::ReconstructedBlock(reconstructed, _) = completed else {
            panic!("the queued transaction reply must complete compact recovery")
        };
        assert_eq!(reconstructed.block(), &block);
        let Message::BlockTransactions(served) = writes.try_recv().unwrap() else {
            panic!("the deferred recovery request precedes the queued heartbeat")
        };
        assert_eq!(served.block_hash, block.hash());
        assert_eq!(served.transactions, vec![block.transactions[0].clone()]);
        assert!(writes.try_recv().is_err());
        assert!(futures::poll!(&mut run).is_pending());

        // Finish the original framed write; it must not be cancelled, repeated, or overtaken.
        release.send(()).unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(writes.try_recv().unwrap(), Message::Pong(Nonce(9)));
        assert!(writes.try_recv().is_err());
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("inbound serving cannot hide the compact recovery deadline");
}

#[tokio::test(start_paused = true)]
async fn compact_recovery_bounds_deferred_messages_and_preserves_deadline_order() {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .as_slice()
        .zcash_deserialize_into()
        .unwrap();
    for mode in ["count", "bytes", "deadline"] {
        let (mut connection, _client, _inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        let response = recovering(&mut connection, &block);
        let messages = if mode == "bytes" {
            // Each inv fits on its own, but together they exceed the one-MiB wire-byte budget.
            vec![Message::Inv(vec![InventoryHash::Block(block.hash()); 15_000]); 2]
        } else {
            (0..if mode == "count" { 65 } else { 64 })
                .map(|nonce| Message::Ping(Nonce(nonce)))
                .collect()
        };
        let peer_rx =
            futures::stream::iter(messages.into_iter().map(Ok)).chain(futures::stream::pending());
        let run = connection.run(peer_rx);
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        assert!(matches!(
            outbound.try_recv(),
            Ok(Message::SendCompact { .. })
        ));
        if mode != "deadline" {
            assert!(futures::poll!(&mut run).is_ready());
            assert!(response.await.unwrap().is_err());
            assert!(errors.try_get_error().is_some());
            assert!(outbound.try_recv().is_err());
            continue;
        }
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert!(futures::poll!(&mut run).is_pending());
        assert!(outbound.try_recv().is_err());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(
            outbound.try_recv().unwrap(),
            Message::GetData(vec![block.hash().into()])
        );
        for nonce in 0..64 {
            assert!(futures::poll!(&mut run).is_pending());
            assert_eq!(outbound.try_recv().unwrap(), Message::Pong(Nonce(nonce)));
        }
        assert!(errors.try_get_error().is_none());
        drop(response);
    }
}

#[tokio::test(start_paused = true)]
async fn compact_discovery_serving_orders_pong_after_backpressured_locator() {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .as_slice()
        .zcash_deserialize_into()
        .unwrap();
    for recovering_peer in [false, true] {
        for empty in [false, true] {
            let (started, mut writes) = tokio::sync::mpsc::channel(4);
            let (release, flushed) = oneshot::channel();
            let sink = Box::pin(futures::sink::unfold(
                Some(flushed),
                move |mut flushed, message| {
                    let inventory = matches!(&message, Message::Inv(_));
                    started.try_send(message).unwrap();
                    async move {
                        if inventory {
                            flushed.take().unwrap().await.unwrap();
                        }
                        Ok::<_, SerializationError>(flushed)
                    }
                },
            ));
            let (mut connection, _client, mut inbound, errors) =
                new_test_connection_with_sink::<PanicAssertion, _>(sink);
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let _response = recovering_peer.then(|| recovering(&mut connection, &block));
            let known_blocks = vec![block.header.previous_block_hash];
            let nonce = Nonce(19);
            let (mut peer, peer_rx) = futures::channel::mpsc::channel(2);
            peer.try_send(Ok(Message::GetBlocks {
                known_blocks: known_blocks.clone(),
                stop: None,
            }))
            .unwrap();
            peer.try_send(Ok(Message::Ping(nonce))).unwrap();
            let run = connection.run(peer_rx);
            tokio::pin!(run);
            assert!(futures::poll!(&mut run).is_pending());
            assert!(matches!(writes.try_recv(), Ok(Message::SendCompact { .. })));
            if recovering_peer {
                // Both messages wait in the recovery queue, still in their original order.
                assert!(futures::poll!(&mut run).is_pending());
                assert!(writes.try_recv().is_err());
                tokio::time::advance(Duration::from_secs(2)).await;
                assert!(futures::poll!(&mut run).is_pending());
                assert_eq!(
                    writes.try_recv().unwrap(),
                    Message::GetData(vec![block.hash().into()])
                );
            }
            assert!(futures::poll!(&mut run).is_pending());
            let request = inbound
                .expect_request(Request::FindBlocks {
                    known_blocks,
                    stop: None,
                })
                .now_or_never()
                .expect("the actual connection calls the locator service before handling its ping");
            assert!(futures::poll!(&mut run).is_pending());
            assert!(
                writes.try_recv().is_err(),
                "a pending inbound service future must prevent the later pong"
            );
            let hashes = if empty {
                Vec::new()
            } else {
                vec![block.hash()]
            };
            request.respond(Response::BlockHashes(hashes.clone()));
            assert!(futures::poll!(&mut run).is_pending());
            if !empty {
                assert_eq!(
                    writes.try_recv().unwrap(),
                    Message::Inv(hashes.into_iter().map(Into::into).collect())
                );
                assert!(futures::poll!(&mut run).is_pending());
                assert!(
                    writes.try_recv().is_err(),
                    "the pong cannot overtake a partially flushed inventory response"
                );
                release.send(()).unwrap();
                assert!(futures::poll!(&mut run).is_pending());
            }
            // Inbound serving yields once after its response, before reading the ping.
            assert!(futures::poll!(&mut run).is_pending());
            assert_eq!(writes.try_recv().unwrap(), Message::Pong(nonce));
            assert!(errors.try_get_error().is_none());
        }
    }
}
