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

fn assert_reconstructed(response: Response, block: &Arc<Block>, source: crate::PeerSocketAddr) {
    let Response::ReconstructedBlock(reconstructed, actual_source) = response else {
        panic!("a compact download must retain its size evidence");
    };
    let (actual_block, size) = reconstructed.into_parts();
    assert_eq!(actual_block, *block);
    assert_eq!(size, block.zcash_serialized_size());
    assert_eq!(actual_source, Some(source));
}

#[tokio::test(start_paused = true)]
async fn screened_compact_discovery_cache_expires() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let tip = block.header.previous_block_hash;
        let (mut connection, _client, mut inbound, _outbound, _errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        assert_eq!(connection.compact.next_block_hash(&tip), None);
        let receive = connection.receive_compact(compact.clone());
        let admit = async {
            inbound
                .expect_request(Request::CheckCompactHeader(compact))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(block.hash(), None))
                .await
                .respond(Response::Nil);
        };
        tokio::join!(receive, admit);
        assert_eq!(connection.compact.next_block_hash(&tip), Some(block.hash()));
        tokio::time::advance(Duration::from_secs(15)).await;
        assert_eq!(
            connection.compact.next_block_hash(&tip),
            None,
            "expired compacts cannot answer discovery even before cache cleanup"
        );
    })
    .await
    .expect("screened compact discovery respects its retention deadline");
}

#[tokio::test(start_paused = true)]
async fn connection_find_blocks_cached_compact_rechecks_ban() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(1), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        let info = Arc::make_mut(&mut connection.connection_info);
        info.early_block_relay = true;
        info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
        let (bans, receiver) = tokio::sync::watch::channel(crate::BanList::default());
        connection.bans_receiver = receiver;
        let receive = connection.receive_compact(compact.clone());
        let admit = async {
            inbound
                .expect_request(Request::CheckCompactHeader(compact))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(block.hash(), Some(source)))
                .await
                .respond(Response::Nil);
        };
        tokio::join!(receive, admit);
        assert_eq!(
            connection
                .compact
                .next_block_hash(&block.header.previous_block_hash),
            Some(block.hash())
        );
        bans.send_modify(|bans| bans.ban(source.ip()));
        let (tx, response) = oneshot::channel();
        connection
            .handle_client_request(
                ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: vec![block.header.previous_block_hash],
                        stop: None,
                    },
                    tx,
                    inv_collector: None,
                    transient_addr: Some(source),
                    span: tracing::Span::none(),
                }
                .into(),
            )
            .await;
        assert!(matches!(
            connection.state,
            crate::peer::connection::State::Failed
        ));
        assert_eq!(
            response.await.unwrap().unwrap_err().inner_debug(),
            "ConnectionDropped"
        );
        assert_eq!(
            errors.try_get_error().unwrap().inner_debug(),
            "ConnectionDropped"
        );
        assert!(outbound.try_recv().is_err());
    })
    .await
    .expect("cached discovery must reject a newly banned peer");
}

#[tokio::test(start_paused = true)]
async fn connection_find_blocks_compact_drains_retired_wire_reply() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let next_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let next_compact = Arc::new(CompactBlock::from_block(&next_block, 8).unwrap());
        for mode in ["pending", "idle", "expired", "second_compact"] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let (mut peer, peer_rx) = mpsc::channel(1);
            let task = tokio::spawn(connection.run(peer_rx));
            assert_eq!(
                outbound.next().await,
                Some(Message::SendCompact {
                    high_bandwidth: true,
                    version: 1,
                })
            );
            let known_blocks = vec![block.header.previous_block_hash];
            let (tx, discovery) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: known_blocks.clone(),
                        stop: None,
                    },
                    tx,
                    inv_collector: None,
                    transient_addr: None,
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            assert_eq!(
                outbound.next().await,
                Some(Message::GetBlocks {
                    known_blocks,
                    stop: None,
                })
            );
            let original_deadline = tokio::time::Instant::now() + crate::constants::REQUEST_TIMEOUT;
            peer.send(Ok(Message::CompactBlock(compact.clone())))
                .await
                .unwrap();
            inbound
                .expect_request(Request::CheckCompactHeader(compact.clone()))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(block.hash(), None))
                .await
                .respond(Response::Nil);
            assert_eq!(
                discovery.await.unwrap().unwrap(),
                Response::BlockHashes(vec![block.hash()])
            );
            let retired_hash = zebra_chain::block::Hash([7; 32]);
            if mode == "idle" {
                peer.send(Ok(Message::Inv(vec![retired_hash.into()])))
                    .await
                    .unwrap();
                inbound
                    .expect_request(Request::AdvertiseBlock(retired_hash, None))
                    .await
                    .respond(Response::Nil);
            }
            // Give the next wire request a deadline later than the retired request's deadline.
            if mode == "expired" {
                tokio::time::advance(Duration::from_secs(1)).await;
            } else if mode == "second_compact" {
                // The compact cooldown expires before the retired wire request's deadline.
                tokio::time::advance(Duration::from_secs(19)).await;
            }
            let known_blocks = if mode == "second_compact" {
                vec![next_block.header.previous_block_hash]
            } else {
                vec![zebra_chain::block::Hash([4; 32])]
            };
            let (tx, mut next_discovery) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: known_blocks.clone(),
                        stop: None,
                    },
                    tx,
                    inv_collector: None,
                    transient_addr: None,
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            assert_eq!(
                outbound.next().await,
                Some(Message::GetBlocks {
                    known_blocks,
                    stop: None,
                })
            );
            if mode == "second_compact" {
                peer.send(Ok(Message::CompactBlock(next_compact.clone())))
                    .await
                    .unwrap();
                inbound
                    .expect_request(Request::CheckCompactHeader(next_compact.clone()))
                    .await
                    .respond(Response::Nil);
                inbound
                    .expect_request(Request::AdvertiseCompactBlock(next_block.hash(), None))
                    .await
                    .respond(Response::Nil);
                assert_eq!(
                    next_discovery.await.unwrap().unwrap(),
                    Response::BlockHashes(vec![next_block.hash()]),
                    "another compact must complete discovery without waiting for an older drain"
                );
                assert!(tokio::time::Instant::now() < original_deadline);
                assert!(errors.try_get_error().is_none());
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                continue;
            }
            if mode == "pending" {
                peer.send(Ok(Message::Inv(vec![retired_hash.into()])))
                    .await
                    .unwrap();
                inbound
                    .expect_request(Request::AdvertiseBlock(retired_hash, None))
                    .await
                    .respond(Response::Nil);
            } else if mode == "expired" {
                tokio::time::advance(original_deadline - tokio::time::Instant::now()).await;
            }
            // The pong proves the retired inventory or its expiry was processed.
            let nonce = crate::types::Nonce::default();
            peer.send(Ok(Message::Ping(nonce))).await.unwrap();
            assert_eq!(outbound.next().await, Some(Message::Pong(nonce)));
            assert!(
                next_discovery.try_recv().unwrap().is_none(),
                "a retired inventory must not complete the next discovery request"
            );
            let hashes = vec![
                zebra_chain::block::Hash([5; 32]),
                zebra_chain::block::Hash([6; 32]),
            ];
            peer.send(Ok(Message::Inv(
                hashes.iter().copied().map(Into::into).collect(),
            )))
            .await
            .unwrap();
            assert_eq!(
                next_discovery.await.unwrap().unwrap(),
                Response::BlockHashes(hashes),
                "only the next wire reply completes the next request, including after drain expiry"
            );
            assert!(errors.try_get_error().is_none());
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    })
    .await
    .expect("retired discovery replies drain once or expire at their original deadline");
}

#[tokio::test(start_paused = true)]
async fn connection_find_blocks_screened_compact_unblocks_reconstruction() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(1), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source = "127.0.0.1:8233".parse().unwrap();
        let start = tokio::time::Instant::now();
        for mode in [
            "unnegotiated",
            "invalid",
            "different_locator",
            "explicit_stop",
            "valid",
        ] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            let info = Arc::make_mut(&mut connection.connection_info);
            info.early_block_relay = mode != "unnegotiated";
            info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
            let (mut peer, peer_rx) = mpsc::channel(1);
            let task = tokio::spawn(connection.run(peer_rx));
            if mode != "unnegotiated" {
                assert_eq!(
                    outbound.next().await,
                    Some(Message::SendCompact {
                        high_bandwidth: true,
                        version: 1
                    })
                );
            }
            let known_blocks = if mode == "different_locator" {
                vec![
                    zebra_chain::block::Hash([0; 32]),
                    block.header.previous_block_hash,
                ]
            } else {
                vec![block.header.previous_block_hash]
            };
            let stop = (mode == "explicit_stop").then_some(block.hash());
            let (tx, mut discovery) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: known_blocks.clone(),
                        stop,
                    },
                    tx,
                    inv_collector: None,
                    transient_addr: Some(source),
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            assert_eq!(
                outbound.next().await,
                Some(Message::GetBlocks { known_blocks, stop })
            );
            // Each sender has one buffer slot even though the connection channel has capacity zero.
            let mut buffered_discovery = Vec::new();
            if mode == "valid" {
                for _ in 0..2 {
                    let (tx, rx) = oneshot::channel();
                    client
                        .clone()
                        .try_send(ClientRequest {
                            request: Request::FindBlocks {
                                known_blocks: vec![block.header.previous_block_hash],
                                stop: None,
                            },
                            tx,
                            inv_collector: None,
                            transient_addr: Some(source),
                            span: tracing::Span::none(),
                        })
                        .unwrap();
                    buffered_discovery.push(rx);
                }
            }
            peer.send(Ok(Message::CompactBlock(compact.clone())))
                .await
                .unwrap();
            if mode != "unnegotiated" {
                let screen = inbound
                    .expect_request(Request::CheckCompactHeader(compact.clone()))
                    .await;
                assert!(
                    discovery.try_recv().unwrap().is_none(),
                    "discovery cannot finish before header screening"
                );
                if mode == "invalid" {
                    screen.respond_error(
                        "compact header does not extend the committed best tip".into(),
                    );
                    inbound
                        .expect_request(Request::AdvertiseBlock(block.hash(), Some(source)))
                        .await
                        .respond(Response::Nil);
                } else {
                    screen.respond(Response::Nil);
                    inbound
                        .expect_request(Request::AdvertiseCompactBlock(block.hash(), Some(source)))
                        .await
                        .respond(Response::Nil);
                }
            }
            // A pong is a barrier proving the compact was handled, not a discovery response.
            let nonce = crate::types::Nonce::default();
            peer.send(Ok(Message::Ping(nonce))).await.unwrap();
            assert_eq!(outbound.next().await, Some(Message::Pong(nonce)));
            if mode != "valid" {
                assert!(
                    discovery.try_recv().unwrap().is_none(),
                    "only a screened compact extending a stop-free locator tip may answer discovery"
                );
                let hashes = vec![
                    zebra_chain::block::Hash([2; 32]),
                    zebra_chain::block::Hash([3; 32]),
                ];
                peer.send(Ok(Message::Inv(
                    hashes.iter().copied().map(Into::into).collect(),
                )))
                .await
                .unwrap();
                assert_eq!(
                    discovery.await.unwrap().unwrap(),
                    Response::BlockHashes(hashes),
                    "discovery remains on the wire until the peer replies"
                );
            } else {
                assert_eq!(
                    discovery.await.unwrap().unwrap(),
                    Response::BlockHashes(vec![block.hash()])
                );
                for response in buffered_discovery {
                    assert_eq!(
                        response.await.unwrap().unwrap(),
                        Response::BlockHashes(vec![block.hash()])
                    );
                }
                assert!(
                    outbound.try_recv().is_err(),
                    "buffered discovery reuses the screened compact without more getblocks"
                );

                // A parent elsewhere in the locator is not enough: only the first hash is the tip.
                for (known_blocks, stop) in [
                    (
                        vec![
                            zebra_chain::block::Hash([0; 32]),
                            block.header.previous_block_hash,
                        ],
                        None,
                    ),
                    (vec![block.header.previous_block_hash], Some(block.hash())),
                ] {
                    let (tx, unmatched) = oneshot::channel();
                    client
                        .send(ClientRequest {
                            request: Request::FindBlocks {
                                known_blocks: known_blocks.clone(),
                                stop,
                            },
                            tx,
                            inv_collector: None,
                            transient_addr: Some(source),
                            span: tracing::Span::none(),
                        })
                        .await
                        .unwrap();
                    assert_eq!(
                        outbound.next().await,
                        Some(Message::GetBlocks { known_blocks, stop })
                    );
                    drop(unmatched);
                }
                let (tx, download) = oneshot::channel();
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
                    .expect_request(Request::CompactBlockTransactions(compact.clone()))
                    .await
                    .respond(Response::CompactBlockTransactions(
                        block.transactions[1..]
                            .iter()
                            .cloned()
                            .map(Into::into)
                            .collect(),
                    ));
                inbound
                    .expect_request(Request::CheckCompactBlock(block.clone()))
                    .await
                    .respond(Response::Nil);
                assert_eq!(
                    download.await.unwrap().unwrap(),
                    Response::EarlyBlocks(vec![InventoryResponse::Available((
                        block.clone(),
                        Some(source)
                    ))])
                );
                assert!(
                    outbound.try_recv().is_err(),
                    "cached reconstruction needs neither getdata nor a committed inv"
                );
            }
            assert!(errors.try_get_error().is_none());
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert_eq!(
            tokio::time::Instant::now(),
            start,
            "discovery and reconstruction must not wait for a timer"
        );
    })
    .await
    .expect("screened compact discovery frees the connection without an inv or timeout");
}

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
                        .expect_request_that(|request| {
                            matches!(request, Request::CheckCompactBlock(checked, Some(evidence))
                                if **checked == *block && evidence.evidence_for(checked).is_some())
                        })
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
            let response = rx.await.unwrap().unwrap();
            if failure == "none" {
                assert_reconstructed(response, &block, source);
            } else {
                assert_eq!(
                    response,
                    Response::EarlyBlocks(vec![InventoryResponse::Available((
                        block.clone(),
                        Some(source)
                    ))])
                );
            }
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
                assert_reconstructed(rx.await.unwrap().unwrap(), &block, source);
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

#[tokio::test(start_paused = true)]
async fn unadmitted_compacts_are_throttled_without_retaining_slots() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        for valid_header in [false, true] {
            let (mut connection, _client, mut inbound, _outbound, _errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let source = connection
                .connection_info
                .connected_addr
                .get_transient_addr();
            for attempt in 0..7 {
                if attempt == 6 {
                    tokio::time::advance(Duration::from_secs(15)).await;
                }
                let receive = connection.receive_compact(compact.clone());
                let screen = async {
                    if attempt == 0 || attempt == 6 {
                        let check = inbound
                            .expect_request(Request::CheckCompactHeader(compact.clone()))
                            .await;
                        if valid_header {
                            check.respond(Response::Nil);
                            inbound
                                .expect_request(Request::AdvertiseCompactBlock(
                                    block.hash(),
                                    source,
                                ))
                                .await
                                .respond_error("duplicate or over-capacity download".into());
                            return;
                        }
                        check.respond_error("known header does not extend the best tip".into());
                    }
                    inbound
                        .expect_request(Request::AdvertiseBlock(block.hash(), source))
                        .await
                        .respond(Response::Nil);
                };
                tokio::join!(receive, screen);
                assert!(!connection.compact.has_cached());
            }
        }
    })
    .await
    .expect("rejected compacts are screened once per cooldown without retaining bodies");
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
async fn compact_receive_demotion_holds_slot_until_write() {
    let _init_guard = zebra_test::init();
    let (mut connection, _client, _inbound, mut outbound, _errors) =
        new_test_connection::<PanicAssertion>();
    let slots = Arc::new(tokio::sync::Semaphore::new(3));
    let _other_sources = slots.clone().try_acquire_many_owned(2).unwrap();
    connection.compact_receive_permit = Some(slots.clone().try_acquire_owned().unwrap());
    let demote = connection.set_compact_receive(None);
    tokio::pin!(demote);
    assert!(futures::poll!(&mut demote).is_pending());
    assert!(
        slots.clone().try_acquire_owned().is_err(),
        "a fourth source must wait for the disabling write",
    );
    let ((), message) = tokio::join!(demote, outbound.next());
    assert_eq!(
        message,
        Some(Message::SendCompact {
            high_bandwidth: false,
            version: 1,
        }),
    );
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn failed_compact_receive_write_releases_slot_before_stalled_close() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        for high_bandwidth in [false, true] {
            let (started, mut writes) = tokio::sync::mpsc::unbounded_channel();
            let sink = Box::pin(futures::sink::unfold((), move |(), message: Message| {
                assert_eq!(
                    message,
                    Message::SendCompact {
                        high_bandwidth,
                        version: 1,
                    },
                );
                started.send(()).unwrap();
                futures::future::pending::<
                    Result<(), zebra_chain::serialization::SerializationError>,
                >()
            }));
            let (mut connection, _client, _inbound, _errors) =
                new_test_connection_with_sink::<PanicAssertion, _>(sink);
            let slots = Arc::new(tokio::sync::Semaphore::new(3));
            let _other_sources = slots.clone().try_acquire_many_owned(2).unwrap();
            let permit = Some(slots.clone().try_acquire_owned().unwrap());
            let permit = if high_bandwidth {
                permit
            } else {
                connection.compact_receive_permit = permit;
                None
            };
            let update = connection.set_compact_receive(permit);
            tokio::pin!(update);
            tokio::select! {
                _ = &mut update => panic!("the transport never flushes"),
                _ = writes.recv() => {}
            }
            assert!(slots.clone().try_acquire_owned().is_err());
            tokio::time::advance(crate::constants::REQUEST_TIMEOUT).await;
            assert!(
                futures::poll!(&mut update).is_pending(),
                "close remains blocked"
            );
            let _replacement = slots
                .clone()
                .try_acquire_owned()
                .expect("failed sources release their slots before closing");
        }
    })
    .await
    .expect("failed receive negotiation cannot withhold replacement source slots");
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
