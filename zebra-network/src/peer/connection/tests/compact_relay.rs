//! Compact recovery uses the normal bounded downloader and falls back without scoring collisions.

use super::{new_test_connection, new_test_connection_with_sink};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock},
    peer::ClientRequest,
    protocol::external::Message,
    InventoryResponse, Request, Response,
};
use futures::{
    channel::{mpsc, oneshot},
    FutureExt, SinkExt, StreamExt,
};
use std::{sync::Arc, time::Duration};
use zebra_chain::{
    block::{Block, MAX_BLOCK_BYTES},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_test::mock_service::PanicAssertion;

#[tokio::test]
async fn compact_recovery_and_collision_safe_fallback_preserve_source() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        assert!(block.transactions.len() > 1);
        let source = "127.0.0.1:8233".parse().unwrap();
        for failure in ["none", "missing", "timeout", "commitments", "canceled"] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            let (recovery, mut active_recovery) = tokio::sync::watch::channel(None);
            connection.compact_recovery = Some(recovery);
            let info = Arc::make_mut(&mut connection.connection_info);
            info.early_block_relay = true;
            info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
            let (mut peer, peer_rx) = mpsc::channel(1);
            let task = tokio::spawn(connection.run(peer_rx));
            assert_eq!(
                outbound.next().await,
                Some(Message::SendCompact {
                    high_bandwidth: true,
                    version: 1
                })
            );
            let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
            peer.send(Ok(Message::CompactBlock(compact.clone())))
                .await
                .unwrap();
            inbound
                .expect_request(Request::CheckCompactHeader(compact.clone()))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(block.hash(), Some(source)))
                .await
                .respond(Response::Nil);
            let (tx, rx) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::BlockDownload {
                        hash: block.hash(),
                        advertiser: Some(source),
                    },
                    tx,
                    inv_collector: None,
                    transient_addr: Some(source),
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            inbound
                .expect_request(Request::CompactBlockTransactions(compact))
                .await
                .respond(Response::CompactBlockTransactions(Vec::new()));
            let Some(Message::GetBlockTransactions(request)) = outbound.next().await else {
                panic!("missing candidates require recovery")
            };
            assert_eq!(request.block_hash, block.hash());
            assert_eq!(
                request.indexes,
                (1..block.transactions.len()).collect::<Vec<_>>()
            );
            active_recovery.changed().await.unwrap();
            assert_eq!(active_recovery.borrow_and_update().unwrap().0, block.hash());
            if failure == "canceled" {
                drop(rx);
                active_recovery.changed().await.unwrap();
                assert!(active_recovery.borrow().is_none());
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                continue;
            }
            if failure != "timeout" {
                let transactions = if failure == "missing" {
                    Vec::new()
                } else {
                    block.transactions[1..].to_vec()
                };
                peer.send(Ok(Message::BlockTransactions(BlockTransactions {
                    block_hash: block.hash(),
                    transactions,
                })))
                .await
                .unwrap();
                if failure != "missing" {
                    let check = inbound
                        .expect_request(Request::CheckCompactBlock(block.clone()))
                        .await;
                    if failure == "commitments" {
                        check.respond_error("authorization commitment mismatch".into());
                    } else {
                        check.respond(Response::Nil);
                    }
                }
            }
            if failure != "none" {
                assert_eq!(
                    outbound.next().await,
                    Some(Message::GetData(vec![block.hash().into()]))
                );
                assert!(active_recovery.borrow().is_none());
                peer.send(Ok(Message::Block(block.clone()))).await.unwrap();
            }
            assert_eq!(
                rx.await.unwrap().unwrap(),
                Response::EarlyBlocks(vec![InventoryResponse::Available((
                    block.clone(),
                    Some(source)
                ))])
            );
            assert!(active_recovery.borrow().is_none());
            if failure == "none" {
                let (tx, rx) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: Request::BlocksByHash([block.hash()].into()),
                        tx,
                        inv_collector: None,
                        transient_addr: Some(source),
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    rx.await.unwrap().unwrap(),
                    Response::EarlyBlocks(vec![InventoryResponse::Available((
                        block.clone(),
                        Some(source)
                    )),])
                );
                assert!(
                    outbound.try_recv().is_err(),
                    "a racing sync request reuses reconstructed bytes"
                );
            }
            assert!(
                errors.try_get_error().is_none(),
                "collision or timeout must not punish a peer"
            );
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    })
    .await
    .expect("bounded recovery completes with either reconstructed or full bytes");
}

#[tokio::test]
async fn compact_negotiation_never_overrides_early_relay_consent() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(2), async {
        let (mut connection, _client, _inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        connection
            .handle_message_as_request(Message::SendCompact {
                high_bandwidth: true,
                version: 1,
            })
            .await;
        let hash = zebra_chain::block::Hash([1; 32]);
        let (result, delivered) = tokio::sync::oneshot::channel();
        let (_, message) = tokio::join!(
            connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash,
                compact_slots: Arc::new(tokio::sync::Semaphore::new(3)),
                result
            }),
            outbound.next()
        );
        assert_eq!(message, Some(Message::Inv(vec![hash.into()])));
        delivered.await.unwrap().unwrap();
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("a non-opted-in connection retains ordinary block gossip");
}

#[tokio::test]
async fn unadmitted_compacts_do_not_retain_global_slots() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(5), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let (mut connection, _client, mut inbound, _outbound, _errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        let source = connection
            .connection_info
            .connected_addr
            .get_transient_addr();
        for valid_header in [false, true] {
            for _ in 0..6 {
                let receive = connection.receive_compact(compact.clone());
                let screen = async {
                    let check = inbound
                        .expect_request(Request::CheckCompactHeader(compact.clone()))
                        .await;
                    if valid_header {
                        check.respond(Response::Nil);
                        inbound
                            .expect_request(Request::AdvertiseCompactBlock(block.hash(), source))
                            .await
                            .respond_error("duplicate or over-capacity download".into());
                    } else {
                        check.respond_error("known header does not extend the best tip".into());
                        inbound
                            .expect_request(Request::AdvertiseBlock(block.hash(), source))
                            .await
                            .respond(Response::Nil);
                    }
                };
                tokio::join!(receive, screen);
                assert!(!connection.compact.has_cached());
            }
        }
    })
    .await
    .expect("rejected headers fall back without retaining compact bodies");
}

#[tokio::test]
async fn only_constructed_compact_adverts_consume_fanout_quota() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let quota = Arc::new(tokio::sync::Semaphore::new(1));
        for mode in [
            "missing",
            "low_bandwidth",
            "unsupported",
            "unavailable",
            "eligible",
            "exhausted",
        ] {
            let (mut connection, _client, mut inbound, mut outbound, _errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            if mode != "missing" {
                connection
                    .handle_message_as_request(Message::SendCompact {
                        high_bandwidth: mode != "low_bandwidth",
                        version: if mode == "unsupported" { 2 } else { 1 },
                    })
                    .await;
            }
            let (result, delivered) = tokio::sync::oneshot::channel();
            let send = connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                compact_slots: quota.clone(),
                result,
            });
            let serve = async {
                if matches!(mode, "unavailable" | "eligible") {
                    let response = if mode == "unavailable" {
                        InventoryResponse::Missing(block.hash())
                    } else {
                        InventoryResponse::Available((block.clone(), None))
                    };
                    inbound
                        .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                        .await
                        .respond(Response::Blocks(vec![response]));
                }
                outbound.next().await.unwrap()
            };
            let (_, message) = tokio::join!(send, serve);
            assert_eq!(
                matches!(message, Message::CompactBlock(_)),
                mode == "eligible"
            );
            delivered.await.unwrap().unwrap();
            assert_eq!(
                quota.available_permits(),
                usize::from(!matches!(mode, "eligible" | "exhausted"))
            );
        }
    })
    .await
    .expect("only delivered compact bodies consume the shared fanout quota");
}

#[tokio::test]
async fn withdrawn_unsent_compact_snapshots_are_not_sent() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let live = Arc::new(AtomicBool::new(true));
        let authorized = live.clone();
        let response = Response::AuthorizedBlocks {
            blocks: vec![InventoryResponse::Available((block.clone(), None))],
            authorization: crate::BlockAuthorization::new(move |_| {
                authorized.load(Ordering::SeqCst)
            }),
        };
        let (result, delivered) = tokio::sync::oneshot::channel();
        let quota = Arc::new(tokio::sync::Semaphore::new(1));
        {
            let send = connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                compact_slots: quota.clone(),
                result,
            });
            tokio::pin!(send);
            assert!(futures::poll!(&mut send).is_pending());
            inbound
                .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                .await
                .respond(response);
            live.store(false, Ordering::SeqCst);
            send.await;
        }
        assert!(delivered.await.unwrap().is_err());
        assert_eq!(quota.available_permits(), 1);
        assert!(!connection.compact.has_cached());
        assert!(
            outbound.try_recv().is_err(),
            "withdrawn unsent snapshots must not reach the wire"
        );
        assert!(
            errors.try_get_error().is_none(),
            "withdrawal is not a peer failure"
        );
    })
    .await
    .expect("compact adverts recheck authorization after detached work");
}

#[tokio::test]
async fn sent_compact_snapshot_survives_withdrawal_with_bounded_recovery() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        assert!(block.transactions.len() > 1);
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let live = Arc::new(AtomicBool::new(true));
        let authorized = live.clone();
        let (result, delivered) = tokio::sync::oneshot::channel();
        let (_, message) = tokio::join!(
            connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                compact_slots: Arc::new(tokio::sync::Semaphore::new(1)),
                result,
            }),
            async {
                inbound
                    .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                    .await
                    .respond(Response::AuthorizedBlocks {
                        blocks: vec![InventoryResponse::Available((block.clone(), None))],
                        authorization: crate::BlockAuthorization::new(move |_| {
                            authorized.load(Ordering::SeqCst)
                        }),
                    });
                outbound.next().await.unwrap()
            }
        );
        assert!(matches!(message, Message::CompactBlock(_)));
        delivered.await.unwrap().unwrap();
        live.store(false, Ordering::SeqCst);
        let until = connection.compact.expiry();
        let unknown = zebra_chain::block::Hash([1; 32]);
        assert!(connection.compact_advert(unknown).await.is_none());
        connection
            .handle_message_as_request(Message::SendCompact {
                high_bandwidth: false,
                version: 1,
            })
            .await;
        assert!(!connection.compact.send);

        Arc::make_mut(&mut connection.connection_info).early_block_relay = false;
        connection
            .handle_message_as_request(Message::GetBlockTransactions(BlockTransactionsRequest {
                block_hash: block.hash(),
                indexes: vec![0],
            }))
            .await;
        assert!(outbound.try_recv().is_err());
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;

        for (hash, indexes) in [
            (unknown, vec![0]),
            (block.hash(), vec![block.transactions.len()]),
            (block.hash(), vec![0, block.transactions.len()]),
            (block.hash(), vec![1, 0]),
            (block.hash(), vec![0, 0]),
            (block.hash(), vec![0, usize::MAX]),
        ] {
            connection
                .handle_message_as_request(Message::GetBlockTransactions(
                    BlockTransactionsRequest {
                        block_hash: hash,
                        indexes,
                    },
                ))
                .await;
            assert!(outbound.try_recv().is_err());
            assert!(connection.compact.has_cached());
        }
        let request = BlockTransactionsRequest {
            block_hash: block.hash(),
            indexes: (0..block.transactions.len()).collect(),
        };
        let expected = BlockTransactions {
            block_hash: block.hash(),
            transactions: block.transactions.clone(),
        };
        let max_responses =
            usize::try_from(MAX_BLOCK_BYTES).unwrap() / expected.zcash_serialized_size();
        assert!(max_responses > 1, "the fixture permits repeated recovery");
        for _ in 0..max_responses {
            let (_, response) = tokio::join!(
                connection.serve_compact_transactions(request.clone()),
                outbound.next()
            );
            assert_eq!(response, Some(Message::BlockTransactions(expected.clone())));
            assert_eq!(connection.compact.expiry(), until);
        }
        assert!(connection
            .serve_compact_transactions(request)
            .now_or_never()
            .is_some());
        assert!(outbound.try_recv().is_err());
        assert!(connection.compact.has_cached());
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("sent snapshots permit repeated recovery within one shared byte budget");
}

#[tokio::test]
async fn compact_serving_deadline_starts_after_successful_delivery() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(40), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let (result, delivered) = tokio::sync::oneshot::channel();
        {
            let send = connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                compact_slots: Arc::new(tokio::sync::Semaphore::new(1)),
                result,
            });
            tokio::pin!(send);
            let message = tokio::select! {
                _ = &mut send => panic!("the unbuffered sink must wait for its receiver"),
                message = async {
                    inbound
                        .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                        .await
                        .respond(Response::Blocks(vec![InventoryResponse::Available((
                            block.clone(),
                            None,
                        ))]));
                    outbound.next().await.unwrap()
                } => message,
            };
            assert!(matches!(message, Message::CompactBlock(_)));
            // Delay completion of the local flush past the construction-time retention window.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(16)).await;
            send.await;
        }
        delivered.await.unwrap().unwrap();
        let until = tokio::time::Instant::now() + Duration::from_secs(15);
        assert_eq!(connection.compact.expiry(), until);
        tokio::time::advance(Duration::from_secs(14)).await;
        let request = BlockTransactionsRequest {
            block_hash: block.hash(),
            indexes: vec![0],
        };
        let (_, response) = tokio::join!(
            connection.serve_compact_transactions(request.clone()),
            outbound.next()
        );
        assert_eq!(
            response,
            Some(Message::BlockTransactions(BlockTransactions {
                block_hash: block.hash(),
                transactions: vec![block.transactions[0].clone()],
            }))
        );
        assert_eq!(connection.compact.expiry(), until);
        tokio::time::advance(Duration::from_secs(1)).await;
        connection.serve_compact_transactions(request).await;
        assert!(outbound.try_recv().is_err());
        assert!(!connection.compact.has_cached());
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("serving lasts fifteen seconds from successful delivery without refresh");
}

#[tokio::test]
async fn failed_compact_advert_releases_fanout_before_stalled_close() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let (started, mut writes) = tokio::sync::mpsc::unbounded_channel();
        let sink = Box::pin(futures::sink::unfold((), move |(), _: Message| {
            started.send(()).unwrap();
            futures::future::pending::<Result<(), zebra_chain::serialization::SerializationError>>()
        }));
        let (mut connection, _client, mut inbound, _errors) =
            new_test_connection_with_sink::<PanicAssertion, _>(sink);
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let quota = Arc::new(tokio::sync::Semaphore::new(1));
        let (result, delivered) = tokio::sync::oneshot::channel();
        let send = connection.send_block_advert(crate::peer_set::BlockAdvert {
            hash: block.hash(),
            compact_slots: quota.clone(),
            result,
        });
        tokio::pin!(send);
        tokio::select! {
            _ = &mut send => panic!("the transport never flushes"),
            _ = async {
                inbound
                    .expect_request(Request::EarlyBlocksByHash([block.hash()].into()))
                    .await
                    .respond(Response::Blocks(vec![InventoryResponse::Available((block, None))]));
                writes.recv().await.unwrap();
            } => {}
        }
        assert_eq!(quota.available_permits(), 0);
        tokio::time::pause();
        tokio::time::advance(crate::constants::REQUEST_TIMEOUT).await;
        tokio::select! {
            _ = &mut send => panic!("close remains blocked"),
            result = delivered => assert!(result.unwrap().is_err()),
        }
        assert!(futures::poll!(&mut send).is_pending());
        assert_eq!(quota.available_permits(), 1);
    })
    .await
    .expect("failed adverts release their quota without waiting for close");
}

#[tokio::test(start_paused = true)]
async fn compact_receive_demotion_releases_slot_before_write() {
    let _init_guard = zebra_test::init();
    let (mut connection, _client, _inbound, _outbound, _errors) =
        new_test_connection::<PanicAssertion>();
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    connection.compact_receive_permit = Some(slots.clone().try_acquire_owned().unwrap());
    let demote = connection.set_compact_receive(None);
    tokio::pin!(demote);
    assert!(futures::poll!(&mut demote).is_pending());
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test]
async fn compact_recovery_expiry_releases_snapshot_before_stalled_close() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let sink = Box::pin(futures::sink::unfold((), |(), _: Message| {
            futures::future::pending::<Result<(), zebra_chain::serialization::SerializationError>>()
        }));
        let (mut connection, _client, mut inbound, _errors) =
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
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(14)).await;
        let recovery = connection.serve_compact_transactions(BlockTransactionsRequest {
            block_hash: block.hash(),
            indexes: vec![0],
        });
        tokio::pin!(recovery);
        assert!(futures::poll!(&mut recovery).is_pending());
        assert_eq!(
            Arc::strong_count(&block),
            2,
            "the pending write owns a body slot"
        );
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert!(
            futures::poll!(&mut recovery).is_pending(),
            "close remains blocked"
        );
        assert_eq!(
            Arc::strong_count(&block),
            1,
            "expiry drops the serving snapshot"
        );
        tokio::time::advance(crate::constants::REQUEST_TIMEOUT).await;
        assert!(futures::poll!(&mut recovery).is_pending());
        assert_eq!(Arc::strong_count(&block), 1);
    })
    .await
    .expect("the retention deadline cancels backpressured recovery before closing");
}
