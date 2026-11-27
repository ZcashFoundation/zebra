//! Compact recovery uses the normal bounded downloader and falls back without scoring collisions.

use super::{
    super::compact_relay::{smaller_announcement, MAX_RECOVERY_REPLIES},
    new_test_connection, new_test_connection_with_sink,
};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock},
    constants::REQUEST_TIMEOUT,
    peer::{ClientRequest, MustUseClientResponseSender},
    protocol::{external::Message, types::Nonce},
    InventoryResponse, PeerError, Request, Response,
};
use futures::{
    channel::{mpsc, oneshot},
    FutureExt, SinkExt, StreamExt,
};
use std::{sync::Arc, time::Duration};
use zebra_chain::{
    block::{Block, MAX_BLOCK_BYTES},
    serialization::{SerializationError, ZcashDeserializeInto, ZcashSerialize},
    transaction::UnminedTx,
};

/// Returns this node's mempool candidates for all the transactions of `block` except the coinbase
/// and the transaction at index 1.
fn all_but_one(block: &Block) -> Vec<UnminedTx> {
    assert!(
        block.transactions.len() > 2,
        "the block has a missing transaction and some candidates"
    );
    block.transactions[2..]
        .iter()
        .cloned()
        .map(UnminedTx::from)
        .collect()
}
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
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            inbound
                .expect_request(Request::CompactBlockTransactions(compact))
                .await
                .respond(Response::CompactBlockTransactions(all_but_one(&block)));
            let Some(Message::GetBlockTransactions(request)) = outbound.next().await else {
                panic!("missing candidates require recovery")
            };
            assert_eq!(request.block_hash, block.hash());
            assert_eq!(request.indexes, vec![1]);
            active_recovery.changed().await.unwrap();
            let expected = active_recovery.borrow_and_update().unwrap();
            assert_eq!(expected.block_hash, block.hash());
            assert_eq!(expected.count, 1);
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
                    vec![block.transactions[1].clone()]
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
            // Downloads from a connection that negotiated compact block relay are marked, whether
            // the block was rebuilt or downloaded in full.
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

/// A block rebuilt from a compact announcement is only returned if the peer still isn't banned,
/// and still meets the minimum version. Rebuilding waits for the inbound service, so the peer is
/// checked after that wait, and before a cached rebuilt block is returned again.
#[tokio::test]
async fn rebuilt_block_downloads_recheck_bans_and_the_minimum_version() {
    use zebra_chain::parameters::{Network, NetworkUpgrade};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        let download = || Request::BlockDownload {
            hash: block.hash(),
            advertiser: Some(source),
        };
        for mode in ["cached", "rebuilt", "recovered"] {
            for revocation in ["ban", "version"] {
                // A connection whose ban list and chain tip the test controls.
                let (mut template, _client, mut inbound, _outbound, _errors) =
                    new_test_connection::<PanicAssertion>();
                let (mut minimum, tip) =
                    crate::peer::MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
                let info = Arc::make_mut(&mut template.connection_info);
                info.early_block_relay = true;
                info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
                info.remote.version = minimum.current();
                let (peer_tx, mut outbound) = mpsc::channel(0);
                let peer_tx =
                    peer_tx.sink_map_err(|_| SerializationError::Parse("test peer closed"));
                let (mut client, client_rx) = mpsc::channel(0);
                let (_adverts, advert_rx) = tokio::sync::mpsc::channel(1);
                let (bans, bans_rx) = tokio::sync::watch::channel(crate::BanList::default());
                let errors = crate::peer::ErrorSlot::default();
                let connection = crate::peer::Connection::new(
                    inbound.clone(),
                    (client_rx, advert_rx, minimum, bans_rx),
                    errors.clone(),
                    peer_tx,
                    crate::peer_set::ActiveConnectionCounter::new_counter().track_connection(),
                    template.connection_info.clone(),
                    Vec::new(),
                );
                let revoke = || {
                    if revocation == "ban" {
                        bans.send_modify(|list| list.ban(source.ip()));
                    } else {
                        // The peer's version is below the minimum version at NU7.
                        tip.send_best_tip_height(
                            NetworkUpgrade::Nu7.activation_height(&Network::Mainnet),
                        );
                    }
                };
                let (mut peer, peer_rx) = mpsc::channel(1);
                let task = tokio::spawn(connection.run(peer_rx));
                assert!(matches!(
                    outbound.next().await,
                    Some(Message::SendCompact { .. })
                ));

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

                let (tx, response) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: download(),
                        tx,
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                let candidates = if mode == "recovered" {
                    all_but_one(&block)
                } else {
                    block.transactions[1..]
                        .iter()
                        .cloned()
                        .map(UnminedTx::from)
                        .collect()
                };
                inbound
                    .expect_request(Request::CompactBlockTransactions(compact.clone()))
                    .await
                    .respond(Response::CompactBlockTransactions(candidates));
                if mode == "recovered" {
                    assert!(matches!(
                        outbound.next().await,
                        Some(Message::GetBlockTransactions(_))
                    ));
                    peer.send(Ok(Message::BlockTransactions(BlockTransactions {
                        block_hash: block.hash(),
                        transactions: vec![block.transactions[1].clone()],
                    })))
                    .await
                    .unwrap();
                }
                let check = inbound
                    .expect_request(Request::CheckCompactBlock(block.clone()))
                    .await;
                let response = if mode == "cached" {
                    check.respond(Response::Nil);
                    assert_eq!(
                        response.await.unwrap().unwrap(),
                        Response::EarlyBlocks(vec![InventoryResponse::Available((
                            block.clone(),
                            Some(source)
                        ))])
                    );
                    revoke();
                    let (tx, response) = oneshot::channel();
                    client
                        .send(ClientRequest {
                            request: download(),
                            tx,
                            span: tracing::Span::none(),
                        })
                        .await
                        .unwrap();
                    response
                } else {
                    // The peer becomes ineligible while its block is checked.
                    revoke();
                    check.respond(Response::Nil);
                    response
                };
                assert_eq!(
                    response.await.unwrap().unwrap_err().inner_debug(),
                    "ConnectionDropped",
                    "{mode} {revocation}"
                );
                task.await.unwrap();
                assert_eq!(
                    errors.try_get_error().unwrap().inner_debug(),
                    "ConnectionDropped",
                    "{mode} {revocation}"
                );
                assert!(
                    outbound.try_recv().is_err(),
                    "{mode} {revocation}: nothing else is requested from the peer"
                );
            }
        }
    })
    .await
    .expect("rebuilt blocks are only returned while the peer is eligible");
}

#[tokio::test]
async fn compact_negotiation_requires_the_service_bit() {
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
            connection.send_block_advert(crate::peer_set::BlockAdvert { hash, result }),
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
async fn only_requested_and_available_blocks_are_announced_in_compact_form() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        for mode in [
            "missing",
            "low_bandwidth",
            "unsupported",
            "unavailable",
            "eligible",
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
                        .expect_request(Request::BlocksByHash([block.hash()].into()))
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
        }
    })
    .await
    .expect("other peers and unavailable blocks keep ordinary inventory announcements");
}

#[tokio::test]
async fn sent_compact_snapshot_serves_bounded_recovery() {
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
        let (result, delivered) = tokio::sync::oneshot::channel();
        let (_, message) = tokio::join!(
            connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                result,
            }),
            async {
                inbound
                    .expect_request(Request::BlocksByHash([block.hash()].into()))
                    .await
                    .respond(Response::Blocks(vec![InventoryResponse::Available((
                        block.clone(),
                        None,
                    ))]));
                outbound.next().await.unwrap()
            }
        );
        assert!(matches!(message, Message::CompactBlock(_)));
        delivered.await.unwrap().unwrap();
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
            (block.hash(), vec![]),
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
        assert!(
            max_responses > MAX_RECOVERY_REPLIES,
            "the byte budget permits more replies than the reply limit"
        );
        for _ in 0..MAX_RECOVERY_REPLIES {
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
    .expect("sent snapshots permit repeated recovery within the reply limit");
}

/// Each delivered compact block announcement answers a few transaction recovery requests, within
/// a byte budget, and a request for no transactions gets no reply.
#[tokio::test]
async fn recovery_replies_are_bounded_per_announcement() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        for (block_bytes, budget) in [
            (
                zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES.as_slice(),
                false,
            ),
            // A block with more than half the byte budget in transactions.
            (
                zebra_test::vectors::BLOCK_TESTNET_141042_BYTES.as_slice(),
                true,
            ),
        ] {
            let block: Arc<Block> = block_bytes.zcash_deserialize_into().unwrap();
            let (mut connection, _client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            connection.compact.send = true;
            let (result, delivered) = tokio::sync::oneshot::channel();
            let (_, message) = tokio::join!(
                connection.send_block_advert(crate::peer_set::BlockAdvert {
                    hash: block.hash(),
                    result,
                }),
                async {
                    inbound
                        .expect_request(Request::BlocksByHash([block.hash()].into()))
                        .await
                        .respond(Response::Blocks(vec![InventoryResponse::Available((
                            block.clone(),
                            None,
                        ))]));
                    outbound.next().await.unwrap()
                }
            );
            assert!(
                matches!(message, Message::CompactBlock(_)),
                "the block is announced in compact form"
            );
            delivered.await.unwrap().unwrap();

            let request = |indexes: Vec<usize>| BlockTransactionsRequest {
                block_hash: block.hash(),
                indexes,
            };
            let reply = |indexes: &[usize]| {
                Message::BlockTransactions(BlockTransactions {
                    block_hash: block.hash(),
                    transactions: indexes
                        .iter()
                        .map(|index| block.transactions[*index].clone())
                        .collect(),
                })
            };
            // A reply waits for the receiver, so a refused request finishes without waiting.
            assert!(
                connection
                    .serve_compact_transactions(request(vec![]))
                    .now_or_never()
                    .is_some(),
                "a request for no transactions gets no reply, and doesn't use one"
            );
            assert!(outbound.try_recv().is_err());

            let all: Vec<usize> = (0..block.transactions.len()).collect();
            let indexes = if budget { all } else { vec![0] };
            let (_, response) = tokio::join!(
                connection.serve_compact_transactions(request(indexes.clone())),
                outbound.next()
            );
            assert_eq!(response, Some(reply(&indexes)));
            if budget {
                assert!(
                    connection
                        .serve_compact_transactions(request(indexes))
                        .now_or_never()
                        .is_some(),
                    "the second full request doesn't fit the byte budget"
                );
                assert!(outbound.try_recv().is_err());
            } else {
                for _ in 1..MAX_RECOVERY_REPLIES {
                    let (_, response) = tokio::join!(
                        connection.serve_compact_transactions(request(vec![1])),
                        outbound.next()
                    );
                    assert_eq!(response, Some(reply(&[1])));
                }
                assert!(
                    connection
                        .serve_compact_transactions(request(vec![1]))
                        .now_or_never()
                        .is_some(),
                    "the reply limit refuses the next request, however small"
                );
                assert!(outbound.try_recv().is_err());
            }
            assert!(connection.compact.has_cached());
            assert!(errors.try_get_error().is_none());
        }
    })
    .await
    .expect("recovery replies are answered without waiting");
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
                result,
            });
            tokio::pin!(send);
            let message = tokio::select! {
                _ = &mut send => panic!("the unbuffered sink must wait for its receiver"),
                message = async {
                    inbound
                        .expect_request(Request::BlocksByHash([block.hash()].into()))
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

/// A `getblocktxn` that arrives late in the snapshot's retention window gets the usual send timeout,
/// instead of failing the connection when the window ends.
#[tokio::test]
async fn late_compact_recovery_gets_the_usual_send_timeout() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let sink = Box::pin(futures::sink::unfold((), |(), _: Message| {
            futures::future::pending::<Result<(), SerializationError>>()
        }));
        let (mut connection, _client, mut inbound, _errors) =
            new_test_connection_with_sink::<PanicAssertion, _>(sink);
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let (advert, ()) = tokio::join!(connection.compact_advert(block.hash()), async {
            inbound
                .expect_request(Request::BlocksByHash([block.hash()].into()))
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
        assert!(futures::poll!(&mut recovery).is_pending());
        assert_eq!(
            Arc::strong_count(&block),
            2,
            "the reply is still being sent after the retention window"
        );
        tokio::time::advance(REQUEST_TIMEOUT).await;
        assert!(
            futures::poll!(&mut recovery).is_pending(),
            "close remains blocked"
        );
        assert_eq!(
            Arc::strong_count(&block),
            1,
            "the timeout drops the serving snapshot"
        );
    })
    .await
    .expect("a stalled reply times out like any other reply");
}

/// A connection that fails drops its compact bodies before its final flush, which a peer that
/// stops reading can block until the connection is dropped.
#[tokio::test]
async fn shutdown_releases_compact_bodies_before_its_final_flush() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        for recovering in [false, true] {
            // The sink takes the `getblocktxn` of a recovery, and then stops.
            let sink = Box::pin(futures::sink::unfold(
                usize::from(recovering),
                |accepted, _: Message| async move {
                    if accepted == 0 {
                        futures::future::pending::<()>().await;
                    }
                    Ok::<_, SerializationError>(accepted - 1)
                },
            ));
            let (mut connection, _client, mut inbound, errors) =
                new_test_connection_with_sink::<PanicAssertion, _>(sink);
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let source = connection
                .connection_info
                .connected_addr
                .get_transient_addr();
            tokio::join!(connection.receive_compact(compact.clone()), async {
                inbound
                    .expect_request(Request::CheckCompactHeader(compact.clone()))
                    .await
                    .respond(Response::Nil);
                inbound
                    .expect_request(Request::AdvertiseCompactBlock(block.hash(), source))
                    .await
                    .respond(Response::Nil);
            });
            assert!(connection.compact.is_cached(block.hash()));

            if recovering {
                let (handler, ()) = tokio::join!(connection.download_block(block.hash()), async {
                    inbound
                        .expect_request(Request::CompactBlockTransactions(compact.clone()))
                        .await
                        .respond(Response::CompactBlockTransactions(all_but_one(&block)));
                });
                let handler = handler.unwrap();
                assert!(matches!(
                    handler,
                    super::super::Handler::CompactBlock { .. }
                ));
                let (tx, _rx) = oneshot::channel();
                connection.state = super::super::State::AwaitingResponse {
                    handler,
                    tx: MustUseClientResponseSender::new(tx),
                    span: tracing::Span::none(),
                };
            }
            assert_eq!(Arc::strong_count(&compact), 2);

            // A write that the peer doesn't read times out, and leaves the sink stalled.
            tokio::time::pause();
            assert!(matches!(
                connection
                    .peer_tx
                    .send(Message::Ping(Nonce::default()))
                    .await,
                Err(PeerError::ConnectionSendTimeout),
            ));
            let shutdown = connection.fail_with(PeerError::ConnectionSendTimeout);
            tokio::pin!(shutdown);
            assert!(
                futures::poll!(&mut shutdown).is_pending(),
                "the final flush is blocked"
            );
            assert!(errors.try_get_error().is_none());
            assert_eq!(
                Arc::strong_count(&compact),
                1,
                "the connection dropped its compact body before the flush"
            );
            tokio::time::resume();
        }
    })
    .await
    .expect("a blocked flush doesn't hold compact bodies");
}

/// A block that a peer announced to this node in compact form is announced back to it as an
/// ordinary inventory, as it would be without compact relay. That holds when the connection kept
/// the announcement, when the downloader didn't admit it, and when it wasn't screened. The peer
/// is registered as a source of the block in each case.
#[tokio::test]
async fn blocks_from_the_peer_are_announced_to_it_as_inventory() {
    use crate::peer_set::InventoryChange;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Announcement {
        Kept,
        NotAdmitted,
        NotScreened,
    }

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source = "127.0.0.1:8233".parse().unwrap();

        for announcement in [
            Announcement::Kept,
            Announcement::NotAdmitted,
            Announcement::NotScreened,
        ] {
            let (mut connection, _client, mut inbound, mut outbound, _errors) =
                new_test_connection::<PanicAssertion>();
            let (inventory, mut registered) = tokio::sync::broadcast::channel(4);
            connection.compact_inventory = Some(inventory);
            let info = Arc::make_mut(&mut connection.connection_info);
            info.early_block_relay = true;
            info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
            connection.compact.send = true;

            tokio::join!(connection.receive_compact(compact.clone()), async {
                let screening = inbound
                    .expect_request(Request::CheckCompactHeader(compact.clone()))
                    .await;
                if announcement == Announcement::NotScreened {
                    screening.respond_error("header does not extend the best tip".into());
                    // The announcement is then handled like a block inventory.
                    inbound
                        .expect_request(Request::AdvertiseBlock(block.hash(), Some(source)))
                        .await
                        .respond(Response::Nil);
                    return;
                }
                screening.respond(Response::Nil);

                let admission = inbound
                    .expect_request(Request::AdvertiseCompactBlock(block.hash(), Some(source)))
                    .await;
                if announcement == Announcement::Kept {
                    admission.respond(Response::Nil);
                } else {
                    admission.respond_error("duplicate or over-capacity download".into());
                }
            });
            assert_eq!(
                connection.compact.is_cached(block.hash()),
                announcement == Announcement::Kept,
                "{announcement:?}",
            );
            assert_eq!(
                registered.try_recv().ok(),
                Some(InventoryChange::new_available(block.hash().into(), source)),
                "{announcement:?}",
            );

            let (result, delivered) = tokio::sync::oneshot::channel();
            let (_, message) = tokio::join!(
                connection.send_block_advert(crate::peer_set::BlockAdvert {
                    hash: block.hash(),
                    result,
                }),
                outbound.next()
            );
            assert_eq!(
                message,
                Some(Message::Inv(vec![block.hash().into()])),
                "{announcement:?}",
            );
            delivered.await.unwrap().unwrap();
            inbound.expect_no_requests().await;
        }
    })
    .await
    .expect("the advert doesn't wait for a compact announcement");
}

/// The writes of a compact download stop at the download's deadline, so a peer that doesn't read
/// can't make the download take longer than one request timeout.
#[tokio::test]
async fn compact_download_writes_stop_at_its_deadline() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        // The peer never reads.
        let sink = Box::pin(futures::sink::unfold((), |(), _: Message| {
            futures::future::pending::<Result<(), SerializationError>>()
        }));
        let (mut connection, _client, mut inbound, _errors) =
            new_test_connection_with_sink::<PanicAssertion, _>(sink);
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        let source = connection
            .connection_info
            .connected_addr
            .get_transient_addr();
        tokio::join!(connection.receive_compact(compact.clone()), async {
            inbound
                .expect_request(Request::CheckCompactHeader(compact.clone()))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(block.hash(), source))
                .await
                .respond(Response::Nil);
        });

        tokio::time::pause();
        let started = tokio::time::Instant::now();
        let (handler, ()) = tokio::join!(connection.download_block(block.hash()), async {
            let candidates = inbound
                .expect_request(Request::CompactBlockTransactions(compact.clone()))
                .await;
            // The inbound service is slow, and then most transactions are missing, so the
            // connection asks for the full block.
            tokio::time::sleep(Duration::from_secs(5)).await;
            candidates.respond(Response::CompactBlockTransactions(Vec::new()));
        });
        assert!(matches!(handler, Err(PeerError::ConnectionSendTimeout)));
        let elapsed = started.elapsed();
        assert!(
            (REQUEST_TIMEOUT..REQUEST_TIMEOUT + Duration::from_millis(100)).contains(&elapsed),
            "the download took {elapsed:?}",
        );
    })
    .await
    .expect("the write stops at the deadline");
}

/// A compact block completes a pending tip poll, like a single block inventory does, and it is
/// still handled as an announcement.
#[tokio::test]
async fn compact_announcement_completes_a_pending_tip_poll() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let parent = block.header.previous_block_hash;
        let source = "127.0.0.1:8233".parse().unwrap();
        let (mut connection, mut client, mut inbound, mut outbound, _errors) =
            new_test_connection::<PanicAssertion>();
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

        let (tx, tip_poll) = oneshot::channel();
        client
            .try_send(ClientRequest {
                request: Request::FindBlocks {
                    known_blocks: vec![parent],
                    stop: None,
                },
                tx,
                span: tracing::Span::none(),
            })
            .expect("the connection is waiting for a request");
        assert_eq!(
            outbound.next().await,
            Some(Message::GetBlocks {
                known_blocks: vec![parent],
                stop: None,
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
        assert_eq!(
            tip_poll.await.unwrap().unwrap(),
            Response::BlockHashes(vec![block.hash()]),
            "the announcement completes the pending tip poll",
        );

        task.abort();
    })
    .await
    .expect("the tip poll doesn't wait for its timeout");
}

/// A compact announcement is registered as the peer's inventory once it has been screened and
/// kept, not when it arrives. Announced blocks hold their own slots, so they don't stop the
/// connection from keeping a peer's compact block.
#[tokio::test]
async fn compact_announcement_is_registered_after_screening_with_its_own_slot() {
    use crate::peer_set::InventoryChange;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let source = "127.0.0.1:8233".parse().unwrap();
        let (mut connection, mut client, mut inbound, mut outbound, _errors) =
            new_test_connection::<PanicAssertion>();
        let (inventory, mut registered) = tokio::sync::broadcast::channel(4);
        connection.compact_inventory = Some(inventory);
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

        // Other connections hold every slot for announcing this node's blocks.
        let sent_slots = super::super::compact_relay::SENT_SLOTS
            .clone()
            .acquire_many_owned(6)
            .await
            .expect("the slots are never closed");

        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        peer.send(Ok(Message::CompactBlock(compact.clone())))
            .await
            .unwrap();
        let screening = inbound
            .expect_request(Request::CheckCompactHeader(compact.clone()))
            .await;
        assert!(
            matches!(
                registered.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "the announcement isn't registered before it is screened",
        );
        screening.respond(Response::Nil);
        inbound
            .expect_request(Request::AdvertiseCompactBlock(block.hash(), Some(source)))
            .await
            .respond(Response::Nil);

        assert_eq!(
            registered.recv().await.expect("the sender is in scope"),
            InventoryChange::new_available(block.hash().into(), source),
        );

        // The connection kept the compact block, so a download of it rebuilds the block.
        let (tx, _rx) = oneshot::channel();
        client
            .send(ClientRequest {
                request: Request::BlockDownload {
                    hash: block.hash(),
                    advertiser: Some(source),
                },
                tx,
                span: tracing::Span::none(),
            })
            .await
            .unwrap();
        inbound
            .expect_request(Request::CompactBlockTransactions(compact))
            .await
            .respond(Response::CompactBlockTransactions(Vec::new()));

        drop(sent_slots);
        task.abort();
    })
    .await
    .expect("the connection handles the announcement");
}

/// When at least half of a compact block's transactions are missing, the block is downloaded in
/// full straight away, instead of recovering its transactions first.
#[tokio::test]
async fn compact_blocks_missing_most_transactions_are_downloaded_in_full() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let source = "127.0.0.1:8233".parse().unwrap();
        let (mut connection, mut client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        let info = Arc::make_mut(&mut connection.connection_info);
        info.early_block_relay = true;
        info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
        let (mut peer, peer_rx) = mpsc::channel(1);
        let task = tokio::spawn(connection.run(peer_rx));
        assert!(matches!(
            outbound.next().await,
            Some(Message::SendCompact { .. })
        ));

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
                span: tracing::Span::none(),
            })
            .await
            .unwrap();
        inbound
            .expect_request(Request::CompactBlockTransactions(compact))
            .await
            .respond(Response::CompactBlockTransactions(Vec::new()));
        assert_eq!(
            outbound.next().await,
            Some(Message::GetData(vec![block.hash().into()])),
            "the block is downloaded in full without a getblocktxn",
        );
        peer.send(Ok(Message::Block(block.clone()))).await.unwrap();
        assert_eq!(
            rx.await.unwrap().unwrap(),
            Response::EarlyBlocks(vec![InventoryResponse::Available((
                block.clone(),
                Some(source)
            ))])
        );
        assert!(errors.try_get_error().is_none());
        task.abort();
    })
    .await
    .expect("the full block is downloaded");
}

/// A block download that starts through compact relay has one request timeout from when it starts,
/// including the time it waits for the inbound service and its fallback to the full block.
#[tokio::test]
async fn compact_download_has_one_request_timeout() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let source = "127.0.0.1:8233".parse().unwrap();
        for recovering in [true, false] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            let info = Arc::make_mut(&mut connection.connection_info);
            info.early_block_relay = true;
            info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
            let (mut peer, peer_rx) = mpsc::channel(1);
            let task = tokio::spawn(connection.run(peer_rx));
            assert!(matches!(
                outbound.next().await,
                Some(Message::SendCompact { .. })
            ));

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

            tokio::time::pause();
            let started = tokio::time::Instant::now();
            let (tx, rx) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::BlockDownload {
                        hash: block.hash(),
                        advertiser: Some(source),
                    },
                    tx,
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            let candidates = inbound
                .expect_request(Request::CompactBlockTransactions(compact))
                .await;
            // The inbound service is slow.
            tokio::time::sleep(Duration::from_secs(5)).await;
            if recovering {
                candidates.respond(Response::CompactBlockTransactions(all_but_one(&block)));
                assert!(matches!(
                    outbound.next().await,
                    Some(Message::GetBlockTransactions(_))
                ));
            } else {
                candidates.respond(Response::CompactBlockTransactions(Vec::new()));
            }
            // The peer doesn't answer the recovery, or the full block request.
            assert_eq!(
                outbound.next().await,
                Some(Message::GetData(vec![block.hash().into()]))
            );
            assert!(rx.await.unwrap().is_err(), "the download times out");
            let elapsed = started.elapsed();
            assert!(
                (REQUEST_TIMEOUT..REQUEST_TIMEOUT + Duration::from_millis(100)).contains(&elapsed),
                "the download took {elapsed:?}",
            );
            assert!(errors.try_get_error().is_none());
            task.abort();
            tokio::time::resume();
        }
    })
    .await
    .expect("the download times out");
}

/// A compact announcement is only sent if it encodes, and its encoding is smaller than the block's.
#[test]
fn compact_announcements_that_fail_to_encode_are_not_sent() {
    let _init_guard = zebra_test::init();
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .as_slice()
        .zcash_deserialize_into()
        .unwrap();
    let compact = CompactBlock::from_block(&block, 7).unwrap();
    assert_eq!(
        smaller_announcement(compact.clone(), &block),
        Some(compact.clone())
    );

    // An announcement of a block with only a coinbase is larger than the block.
    let mut coinbase_only = block.clone();
    coinbase_only.transactions.truncate(1);
    let larger = CompactBlock::from_block(&coinbase_only, 7).unwrap();
    assert_eq!(smaller_announcement(larger, &coinbase_only), None);

    // An announcement whose encoding fails isn't sent.
    let mut unencodable = compact;
    unencodable.prefilled.clear();
    assert!(unencodable.zcash_serialize_to_vec().is_err());
    assert_eq!(smaller_announcement(unencodable, &block), None);
}

/// A block of exactly `MAX_BLOCK_BYTES` that is filled with the smallest transparent transactions
/// is announced in compact form.
#[tokio::test]
async fn full_blocks_of_the_smallest_transactions_are_announced_in_compact_form() {
    let _init_guard = zebra_test::init();
    let block = Arc::new(crate::compact::tests::full_block_of_the_smallest_transactions());
    tokio::time::timeout(Duration::from_secs(60), async {
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let (result, delivered) = tokio::sync::oneshot::channel();
        let (_, message) = tokio::join!(
            connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                result,
            }),
            async {
                inbound
                    .expect_request(Request::BlocksByHash([block.hash()].into()))
                    .await
                    .respond(Response::Blocks(vec![InventoryResponse::Available((
                        block.clone(),
                        None,
                    ))]));
                outbound.next().await.unwrap()
            }
        );
        assert_eq!(
            message.command(),
            "cmpctblock",
            "the block is announced in compact form"
        );
        if let Message::CompactBlock(compact) = message {
            assert_eq!(compact.block_hash(), block.hash());
            assert_eq!(compact.short_ids.len(), block.transactions.len() - 1);
        }
        delivered.await.unwrap().unwrap();
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("full blocks of the smallest transactions are announced in compact form");
}

/// A block that can't be announced in compact form gets an ordinary block inventory, and the
/// connection doesn't keep the block or fail.
#[tokio::test]
async fn blocks_without_a_compact_announcement_fall_back_to_block_inventory() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        // An announcement prefills the coinbase, so it can't be built for a block without one.
        block.transactions.remove(0);
        let block = Arc::new(block);
        let (mut connection, _client, mut inbound, mut outbound, errors) =
            new_test_connection::<PanicAssertion>();
        Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
        connection.compact.send = true;
        let (result, delivered) = tokio::sync::oneshot::channel();
        let (_, message) = tokio::join!(
            connection.send_block_advert(crate::peer_set::BlockAdvert {
                hash: block.hash(),
                result,
            }),
            async {
                inbound
                    .expect_request(Request::BlocksByHash([block.hash()].into()))
                    .await
                    .respond(Response::Blocks(vec![InventoryResponse::Available((
                        block.clone(),
                        None,
                    ))]));
                outbound.next().await.unwrap()
            }
        );
        assert_eq!(message, Message::Inv(vec![block.hash().into()]));
        delivered.await.unwrap().unwrap();
        assert!(!connection.compact.has_cached());
        assert_eq!(Arc::strong_count(&block), 1, "the block isn't kept");
        assert!(errors.try_get_error().is_none());
    })
    .await
    .expect("blocks without a compact announcement keep ordinary inventory announcements");
}
