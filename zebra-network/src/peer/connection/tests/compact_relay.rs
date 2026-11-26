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
        for delay in [0, 15] {
            let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
            let tip = block.header.previous_block_hash;
            let (mut connection, _client, mut inbound, _outbound, _errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let receive = connection.receive_compact(compact.clone());
            let admit = async {
                inbound
                    .expect_request(Request::CheckCompactHeader(compact))
                    .await
                    .respond(Response::Nil);
                inbound
                    .expect_request(Request::AdvertiseCompactBlock(
                        block.hash(),
                        None,
                        block.header.previous_block_hash,
                    ))
                    .await
                    .respond(Response::Nil);
            };
            tokio::join!(receive, admit);
            tokio::time::advance(Duration::from_secs(delay)).await;
            assert_eq!(
                connection.compact.take_discovery(&tip),
                (delay == 0).then(|| vec![block.hash()])
            );
            assert_eq!(connection.compact.take_discovery(&tip), None);
        }
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
        let advertiser = Some(connection.connection_info.clone());
        let (bans, receiver) = tokio::sync::watch::channel(crate::BanList::default());
        connection.bans_receiver = receiver;
        let receive = connection.receive_compact(compact.clone());
        let admit = async {
            inbound
                .expect_request(Request::CheckCompactHeader(compact))
                .await
                .respond(Response::Nil);
            inbound
                .expect_request(Request::AdvertiseCompactBlock(
                    block.hash(),
                    advertiser,
                    block.header.previous_block_hash,
                ))
                .await
                .respond(Response::Nil);
        };
        tokio::join!(receive, admit);
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
async fn connection_find_blocks_inflight_compact_rechecks_authorization() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(5), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        for mode in ["ban_screen", "version_screen", "ban_admit", "version_admit"] {
            let (mut template, _client, mut inbound, _outbound, _errors) =
                new_test_connection::<PanicAssertion>();
            let (mut minimum, tip) = crate::peer::MinimumPeerVersion::with_mock_chain_tip(
                &zebra_chain::parameters::Network::Mainnet,
            );
            tip.send_best_tip_height(Some(zebra_chain::block::Height(1_687_107)));
            let info = Arc::make_mut(&mut template.connection_info);
            info.early_block_relay = true;
            info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
            info.remote.version = minimum.current();
            let (peer_tx, mut outbound) = mpsc::channel(0);
            let peer_tx = peer_tx.sink_map_err(|_| {
                zebra_chain::serialization::SerializationError::Parse("test peer closed")
            });
            let (_client, client_rx) = mpsc::channel(0);
            let (_adverts, advert_rx) = tokio::sync::mpsc::channel(1);
            let (bans, bans_rx) = tokio::sync::watch::channel(crate::BanList::default());
            let errors = crate::peer::ErrorSlot::default();
            let mut connection = crate::peer::Connection::new(
                inbound.clone(),
                (
                    client_rx,
                    advert_rx,
                    tokio::sync::watch::channel(None).1,
                    minimum,
                    bans_rx,
                ),
                errors.clone(),
                peer_tx,
                crate::peer_set::ActiveConnectionCounter::new_counter().track_connection(),
                template.connection_info.clone(),
                Vec::new(),
            );
            let (tx, response) = oneshot::channel();
            let send = connection.handle_client_request(
                ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: vec![block.header.previous_block_hash],
                        stop: None,
                    },
                    tx,
                    span: tracing::Span::none(),
                }
                .into(),
            );
            let ((), request) = tokio::join!(send, outbound.next());
            assert!(matches!(request, Some(Message::GetBlocks { .. })));
            let revoke = || {
                if mode.starts_with("ban") {
                    bans.send_modify(|bans| bans.ban(source.ip()));
                } else {
                    // NU6.1 is below Mainnet's initial NU6.2 minimum, so it would revoke nothing.
                    tip.send_best_tip_height(
                        zebra_chain::parameters::NetworkUpgrade::Nu7
                            .activation_height(&zebra_chain::parameters::Network::Mainnet),
                    );
                }
            };
            let receive = connection.receive_compact(compact.clone());
            let screen = async {
                let header = inbound
                    .expect_request(Request::CheckCompactHeader(compact.clone()))
                    .await;
                if mode.ends_with("screen") {
                    revoke();
                    header.respond(Response::Nil);
                } else {
                    header.respond(Response::Nil);
                    assert!(matches!(outbound.next().await, Some(Message::Ping(_))));
                    let admission = inbound
                        .expect_request(Request::AdvertiseCompactBlock(
                            block.hash(),
                            Some(template.connection_info.clone()),
                            block.header.previous_block_hash,
                        ))
                        .await;
                    revoke();
                    admission.respond(Response::Nil);
                }
            };
            tokio::join!(receive, screen);
            assert_eq!(
                response.await.unwrap().unwrap_err().inner_debug(),
                "ConnectionDropped"
            );
            assert_eq!(
                errors.try_get_error().unwrap().inner_debug(),
                "ConnectionDropped"
            );
            assert!(outbound.try_recv().is_err());
        }
    })
    .await
    .expect("in-flight compact discovery rechecks bans and the live minimum version");
}

#[tokio::test(start_paused = true)]
async fn connection_find_blocks_compact_uses_ordered_pong_barrier() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let parent = block.header.previous_block_hash;
        for mode in ["empty", "gossip", "old", "multi", "heartbeat", "timeout"] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let (mut peer, peer_rx) = mpsc::channel(1);
            let task = tokio::spawn(connection.run(peer_rx));
            assert!(matches!(
                outbound.next().await,
                Some(Message::SendCompact { .. })
            ));
            let (tx, discovery) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::FindBlocks {
                        known_blocks: vec![parent],
                        stop: None,
                    },
                    tx,
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            assert!(matches!(
                outbound.next().await,
                Some(Message::GetBlocks { .. })
            ));
            peer.send(Ok(Message::CompactBlock(compact.clone())))
                .await
                .unwrap();
            inbound
                .expect_request(Request::CheckCompactHeader(compact.clone()))
                .await
                .respond(Response::Nil);
            let Some(Message::Ping(barrier)) = outbound.next().await else {
                panic!("early discovery must place an owned ping after its getblocks")
            };
            inbound
                .expect_request(Request::AdvertiseCompactBlock(
                    block.hash(),
                    None,
                    block.header.previous_block_hash,
                ))
                .await
                .respond(Response::Nil);
            assert_eq!(
                discovery.await.unwrap().unwrap(),
                Response::BlockHashes(vec![block.hash()])
            );
            if mode == "timeout" {
                tokio::time::advance(crate::constants::REQUEST_TIMEOUT).await;
                task.await.unwrap();
                assert_eq!(
                    errors.try_get_error().unwrap().inner_debug(),
                    "ConnectionReceiveTimeout"
                );
                continue;
            }
            if mode == "heartbeat" {
                let (tx, mut heartbeat) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: Request::Ping(barrier),
                        tx,
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                let Some(Message::Ping(heartbeat_nonce)) = outbound.next().await else {
                    panic!("a heartbeat still goes on the wire")
                };
                assert_ne!(heartbeat_nonce, barrier);
                peer.send(Ok(Message::Pong(barrier))).await.unwrap();
                let marker = crate::types::Nonce(11);
                peer.send(Ok(Message::Ping(marker))).await.unwrap();
                assert_eq!(outbound.next().await, Some(Message::Pong(marker)));
                assert!(heartbeat.try_recv().unwrap().is_none());
                peer.send(Ok(Message::Pong(heartbeat_nonce))).await.unwrap();
                assert!(matches!(
                    heartbeat.await.unwrap().unwrap(),
                    Response::Pong(_)
                ));
            }
            let hashes = vec![block.hash(), zebra_chain::block::Hash([7; 32])];
            let mut next_discovery = None;
            if mode != "multi" {
                let (tx, response) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: Request::FindBlocks {
                            known_blocks: vec![parent],
                            stop: None,
                        },
                        tx,
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                if mode == "heartbeat" {
                    // The barrier's pong already arrived, so the request is sent at once.
                    assert!(matches!(
                        outbound.next().await,
                        Some(Message::GetBlocks { .. })
                    ));
                } else {
                    // The request waits for the barrier's pong.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    assert!(outbound.try_recv().is_err());
                }
                next_discovery = Some(response);
            }
            if mode == "gossip" {
                let gossip = zebra_chain::block::Hash([9; 32]);
                peer.send(Ok(Message::Inv(vec![gossip.into()])))
                    .await
                    .unwrap();
                inbound
                    .expect_request(Request::AdvertiseBlock(gossip, None))
                    .await
                    .respond(Response::Nil);
            }
            if mode == "old" || mode == "gossip" {
                peer.send(Ok(Message::Inv(vec![
                    zebra_chain::block::Hash([5; 32]).into(),
                    zebra_chain::block::Hash([6; 32]).into(),
                ])))
                .await
                .unwrap();
            } else if mode == "multi" {
                peer.send(Ok(Message::Inv(
                    hashes.iter().copied().map(Into::into).collect(),
                )))
                .await
                .unwrap();
            }
            if mode != "heartbeat" {
                peer.send(Ok(Message::Pong(crate::types::Nonce(
                    barrier.0.wrapping_add(1),
                ))))
                .await
                .unwrap();
                let marker = crate::types::Nonce(12);
                peer.send(Ok(Message::Ping(marker))).await.unwrap();
                assert_eq!(outbound.next().await, Some(Message::Pong(marker)));
                if let Some(response) = &mut next_discovery {
                    assert!(response.try_recv().unwrap().is_none());
                }
                peer.send(Ok(Message::Pong(barrier))).await.unwrap();
                if mode != "multi" {
                    assert!(
                        matches!(outbound.next().await, Some(Message::GetBlocks { .. })),
                        "the waiting request is sent after the barrier's pong"
                    );
                }
            }
            if mode == "multi" {
                let (tx, response) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: Request::FindBlocks {
                            known_blocks: vec![parent],
                            stop: None,
                        },
                        tx,
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    response.await.unwrap().unwrap(),
                    Response::BlockHashes(hashes.clone())
                );
                assert!(outbound.try_recv().is_err());
                let (tx, response) = oneshot::channel();
                client
                    .send(ClientRequest {
                        request: Request::FindBlocks {
                            known_blocks: vec![parent],
                            stop: None,
                        },
                        tx,
                        span: tracing::Span::none(),
                    })
                    .await
                    .unwrap();
                assert!(matches!(
                    outbound.next().await,
                    Some(Message::GetBlocks { .. })
                ));
                next_discovery = Some(response);
            }
            let progressed = vec![hashes[0], hashes[1], zebra_chain::block::Hash([8; 32])];
            peer.send(Ok(Message::Inv(
                progressed.iter().copied().map(Into::into).collect(),
            )))
            .await
            .unwrap();
            assert_eq!(
                next_discovery.unwrap().await.unwrap().unwrap(),
                Response::BlockHashes(progressed)
            );
            assert!(errors.try_get_error().is_none());
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    })
    .await
    .expect("only the owned pong retires discovery, including an empty old result");
}

/// A running connection whose block locator request was answered by a screened compact block.
/// Its discovery barrier is up until the peer answers the barrier's ping.
struct BarrierConnection {
    task: tokio::task::JoinHandle<()>,
    client: mpsc::Sender<ClientRequest>,
    inbound: zebra_test::mock_service::MockService<Request, Response, PanicAssertion>,
    outbound: mpsc::Receiver<Message>,
    peer: mpsc::Sender<Result<Message, zebra_chain::serialization::SerializationError>>,
    errors: crate::peer::ErrorSlot,
    bans: tokio::sync::watch::Sender<crate::BanList>,
    tip: zebra_chain::chain_tip::mock::MockChainTipSender,
    advertiser: Option<Arc<crate::peer::ConnectionInfo>>,
    barrier: crate::types::Nonce,
}

impl BarrierConnection {
    /// The peer's address.
    const SOURCE: &'static str = "127.0.0.1:8233";

    /// Starts a connection, and answers its block locator request with `compact`.
    async fn start(compact: &Arc<CompactBlock>) -> Self {
        let source: crate::PeerSocketAddr = Self::SOURCE.parse().unwrap();
        let (mut template, _client, mut inbound, _outbound, _errors) =
            new_test_connection::<PanicAssertion>();
        let (mut minimum, tip) = crate::peer::MinimumPeerVersion::with_mock_chain_tip(
            &zebra_chain::parameters::Network::Mainnet,
        );
        let info = Arc::make_mut(&mut template.connection_info);
        info.early_block_relay = true;
        info.connected_addr = crate::ConnectedAddr::new_outbound_direct(source);
        info.remote.version = minimum.current();
        let advertiser = Some(template.connection_info.clone());
        let (peer_tx, mut outbound) = mpsc::channel(0);
        let peer_tx = peer_tx.sink_map_err(|_| {
            zebra_chain::serialization::SerializationError::Parse("test peer closed")
        });
        let (mut client, client_rx) = mpsc::channel(0);
        let (_adverts, advert_rx) = tokio::sync::mpsc::channel(1);
        let (bans, bans_rx) = tokio::sync::watch::channel(crate::BanList::default());
        let errors = crate::peer::ErrorSlot::default();
        let connection = crate::peer::Connection::new(
            inbound.clone(),
            (
                client_rx,
                advert_rx,
                tokio::sync::watch::channel(None).1,
                minimum,
                bans_rx,
            ),
            errors.clone(),
            peer_tx,
            crate::peer_set::ActiveConnectionCounter::new_counter().track_connection(),
            template.connection_info.clone(),
            Vec::new(),
        );
        // Buffer a few messages, so the test can send messages while the connection waits.
        let (mut peer, peer_rx) = mpsc::channel(8);
        let task = tokio::spawn(connection.run(peer_rx));
        assert!(matches!(
            outbound.next().await,
            Some(Message::SendCompact { .. })
        ));

        let parent = compact.header.previous_block_hash;
        let (tx, discovery) = oneshot::channel();
        client
            .send(ClientRequest {
                request: Request::FindBlocks {
                    known_blocks: vec![parent],
                    stop: None,
                },
                tx,
                span: tracing::Span::none(),
            })
            .await
            .unwrap();
        assert!(matches!(
            outbound.next().await,
            Some(Message::GetBlocks { .. })
        ));
        peer.send(Ok(Message::CompactBlock(compact.clone())))
            .await
            .unwrap();
        inbound
            .expect_request(Request::CheckCompactHeader(compact.clone()))
            .await
            .respond(Response::Nil);
        let Some(Message::Ping(barrier)) = outbound.next().await else {
            panic!("a screened compact that answers discovery sends a barrier ping")
        };
        inbound
            .expect_request(Request::AdvertiseCompactBlock(
                compact.header.hash(),
                advertiser.clone(),
                parent,
            ))
            .await
            .respond(Response::Nil);
        assert_eq!(
            discovery.await.unwrap().unwrap(),
            Response::BlockHashes(vec![compact.header.hash()])
        );

        Self {
            task,
            client,
            inbound,
            outbound,
            peer,
            errors,
            bans,
            tip,
            advertiser,
            barrier,
        }
    }

    /// Sends `request` to the connection, and returns its response receiver.
    async fn request(
        &mut self,
        request: Request,
    ) -> oneshot::Receiver<Result<Response, crate::SharedPeerError>> {
        let (tx, response) = oneshot::channel();
        self.client
            .send(ClientRequest {
                request,
                tx,
                span: tracing::Span::none(),
            })
            .await
            .unwrap();
        response
    }

    /// Sends `message` from the peer.
    async fn receive(&mut self, message: Message) {
        self.peer.send(Ok(message)).await.unwrap();
    }
}

/// The barrier's pong, and the replies to the block locator request it retired, are consumed
/// while a compact block's missing transactions are recovered, so they don't use the recovery's
/// limit on deferred messages.
#[tokio::test(start_paused = true)]
async fn discovery_barrier_replies_do_not_use_the_recovery_deferral_limit() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let start = tokio::time::Instant::now();
        let mut connection = BarrierConnection::start(&compact).await;

        // Start recovering the compact block's transactions.
        let download = connection
            .request(Request::BlockDownload {
                hash: block.hash(),
                advertiser: connection.advertiser.clone(),
            })
            .await;
        connection
            .inbound
            .expect_request(Request::CompactBlockTransactions(compact.clone()))
            .await
            .respond(Response::CompactBlockTransactions(Vec::new()));
        assert!(matches!(
            connection.outbound.next().await,
            Some(Message::GetBlockTransactions(_))
        ));

        // Unrelated messages wait until recovery finishes, up to the deferral limit.
        for _ in 1..crate::peer::connection::MAX_DEFERRED_MESSAGES {
            connection.receive(Message::Inv(Vec::new())).await;
        }
        let barrier = connection.barrier;
        connection.receive(Message::Pong(barrier)).await;
        connection
            .receive(Message::Inv(vec![
                block.hash().into(),
                zebra_chain::block::Hash([7; 32]).into(),
            ]))
            .await;
        connection
            .receive(Message::BlockTransactions(BlockTransactions {
                block_hash: block.hash(),
                transactions: block.transactions[1..].to_vec(),
            }))
            .await;
        connection
            .inbound
            .expect_request_that(|request| {
                matches!(request, Request::CheckCompactBlock(checked, Some(evidence))
                    if **checked == *block && evidence.evidence_for(checked).is_some())
            })
            .await
            .respond(Response::Nil);
        assert_reconstructed(
            download.await.unwrap().unwrap(),
            &block,
            BarrierConnection::SOURCE.parse().unwrap(),
        );
        assert!(connection.errors.try_get_error().is_none());
        assert_eq!(
            tokio::time::Instant::now(),
            start,
            "recovery finishes without its fallback timer"
        );
        connection.task.abort();
    })
    .await
    .expect("recovery completes without timers");
}

/// While the discovery barrier is up and no request is waiting, the replies to the retired block
/// locator request are consumed, and single-block inventories for other blocks are still block
/// announcements.
#[tokio::test(start_paused = true)]
async fn discovery_barrier_keeps_other_block_announcements() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let mut connection = BarrierConnection::start(&compact).await;

        // The previous reply can name only the compact's block, or extend it.
        let extension = vec![block.hash(), zebra_chain::block::Hash([7; 32])];
        connection
            .receive(Message::Inv(vec![block.hash().into()]))
            .await;
        connection
            .receive(Message::Inv(
                extension.iter().copied().map(Into::into).collect(),
            ))
            .await;
        // An announcement of another block still reaches the inbound service, and it's the first
        // request the inbound service gets.
        let other = zebra_chain::block::Hash([9; 32]);
        connection.receive(Message::Inv(vec![other.into()])).await;
        connection
            .inbound
            .expect_request(Request::AdvertiseBlock(
                other,
                connection.advertiser.clone(),
            ))
            .await
            .respond(Response::Nil);
        let barrier = connection.barrier;
        connection.receive(Message::Pong(barrier)).await;

        // The extension answers the next block locator request.
        let response = connection
            .request(Request::FindBlocks {
                known_blocks: vec![block.header.previous_block_hash],
                stop: None,
            })
            .await;
        assert_eq!(
            response.await.unwrap().unwrap(),
            Response::BlockHashes(extension)
        );
        assert!(connection.outbound.try_recv().is_err());
        connection.inbound.expect_no_requests().await;
        assert!(connection.errors.try_get_error().is_none());
        connection.task.abort();
    })
    .await
    .expect("the barrier's replies are handled without timers");
}

/// While the discovery barrier is up, a block locator request waits for the barrier's pong,
/// because the peer's block inventories before that pong reply to an earlier request. After the
/// pong, the request is sent, or answered with an extension of the earlier request, if the peer is
/// still eligible.
#[tokio::test(start_paused = true)]
async fn block_locator_request_waits_for_the_discovery_barrier() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
        let source: crate::PeerSocketAddr = BarrierConnection::SOURCE.parse().unwrap();
        let locator = vec![block.header.previous_block_hash];
        let extension = vec![block.hash(), zebra_chain::block::Hash([7; 32])];
        let earlier_reply = vec![
            zebra_chain::block::Hash([5; 32]),
            zebra_chain::block::Hash([6; 32]),
        ];
        for mode in [
            "reply",
            "extension",
            "ban",
            "version",
            "canceled",
            "timeout",
        ] {
            let mut connection = BarrierConnection::start(&compact).await;
            let start = tokio::time::Instant::now();
            let mut response = connection
                .request(Request::FindBlocks {
                    known_blocks: locator.clone(),
                    stop: None,
                })
                .await;
            // Let the connection take the request.
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(
                connection.outbound.try_recv().is_err(),
                "{mode}: the request waits for the barrier's pong"
            );

            // A reply to the earlier request doesn't answer the waiting request.
            let earlier = if matches!(mode, "extension" | "ban") {
                &extension
            } else {
                &earlier_reply
            };
            connection
                .receive(Message::Inv(
                    earlier.iter().copied().map(Into::into).collect(),
                ))
                .await;
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(response.try_recv().unwrap().is_none(), "{mode}");

            match mode {
                "ban" => connection.bans.send_modify(|bans| bans.ban(source.ip())),
                "version" => connection.tip.send_best_tip_height(
                    zebra_chain::parameters::NetworkUpgrade::Nu7
                        .activation_height(&zebra_chain::parameters::Network::Mainnet),
                ),
                "canceled" => {
                    drop(response);
                    let barrier = connection.barrier;
                    connection.receive(Message::Pong(barrier)).await;
                    // Nothing is sent for the canceled request, and the connection stays open.
                    let marker = crate::types::Nonce(13);
                    connection.receive(Message::Ping(marker)).await;
                    assert_eq!(
                        connection.outbound.next().await,
                        Some(Message::Pong(marker))
                    );
                    assert!(connection.errors.try_get_error().is_none());
                    connection.task.abort();
                    continue;
                }
                "timeout" => {
                    // Without the barrier's pong, the connection and the request time out.
                    tokio::time::advance(crate::constants::REQUEST_TIMEOUT).await;
                    assert_eq!(
                        response.await.unwrap().unwrap_err().inner_debug(),
                        "ConnectionReceiveTimeout"
                    );
                    connection.task.await.unwrap();
                    assert_eq!(
                        connection.errors.try_get_error().unwrap().inner_debug(),
                        "ConnectionReceiveTimeout"
                    );
                    continue;
                }
                _ => {}
            }
            let barrier = connection.barrier;
            connection.receive(Message::Pong(barrier)).await;
            match mode {
                "reply" => {
                    assert_eq!(
                        connection.outbound.next().await,
                        Some(Message::GetBlocks {
                            known_blocks: locator.clone(),
                            stop: None,
                        })
                    );
                    let reply = vec![
                        zebra_chain::block::Hash([2; 32]),
                        zebra_chain::block::Hash([3; 32]),
                    ];
                    connection
                        .receive(Message::Inv(
                            reply.iter().copied().map(Into::into).collect(),
                        ))
                        .await;
                    assert_eq!(
                        response.await.unwrap().unwrap(),
                        Response::BlockHashes(reply)
                    );
                    assert!(
                        tokio::time::Instant::now() - start < crate::constants::REQUEST_TIMEOUT,
                        "the reply answers the request before it times out"
                    );
                    assert!(connection.errors.try_get_error().is_none());
                }
                "extension" => {
                    assert_eq!(
                        response.await.unwrap().unwrap(),
                        Response::BlockHashes(extension.clone())
                    );
                    assert!(connection.outbound.try_recv().is_err());
                    assert!(connection.errors.try_get_error().is_none());
                }
                "ban" | "version" => {
                    assert_eq!(
                        response.await.unwrap().unwrap_err().inner_debug(),
                        "ConnectionDropped",
                        "{mode}"
                    );
                    assert_eq!(
                        connection.errors.try_get_error().unwrap().inner_debug(),
                        "ConnectionDropped",
                        "{mode}"
                    );
                    assert!(connection.outbound.try_recv().is_err(), "{mode}");
                }
                _ => unreachable!("every mode is handled"),
            }
            connection.task.abort();
        }
    })
    .await
    .expect("a waiting block locator request is handled without request timeouts");
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
            let advertiser = Some(connection.connection_info.clone());
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
                    span: tracing::Span::none(),
                })
                .await
                .unwrap();
            assert_eq!(
                outbound.next().await,
                Some(Message::GetBlocks { known_blocks, stop })
            );
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
                        .expect_request(Request::AdvertiseCompactBlock(
                            block.hash(),
                            advertiser.clone(),
                            block.header.previous_block_hash,
                        ))
                        .await
                        .respond(Response::Nil);
                } else {
                    screen.respond(Response::Nil);
                    if mode == "valid" {
                        let Some(Message::Ping(barrier)) = outbound.next().await else {
                            panic!("early discovery sends an ordering barrier")
                        };
                        peer.send(Ok(Message::Pong(barrier))).await.unwrap();
                    }
                    inbound
                        .expect_request(Request::AdvertiseCompactBlock(
                            block.hash(),
                            advertiser.clone(),
                            block.header.previous_block_hash,
                        ))
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
                            advertiser: advertiser.clone(),
                        },
                        tx,
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
                    .expect_request_that(|request| {
                        matches!(request, Request::CheckCompactBlock(checked, Some(evidence))
                            if **checked == *block && evidence.evidence_for(checked).is_some())
                    })
                    .await
                    .respond(Response::Nil);
                assert_reconstructed(download.await.unwrap().unwrap(), &block, source);
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
            let advertiser = Some(connection.connection_info.clone());
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
                .expect_request(Request::AdvertiseCompactBlock(
                    block.hash(),
                    advertiser.clone(),
                    block.header.previous_block_hash,
                ))
                .await
                .respond(Response::Nil);
            let (tx, rx) = oneshot::channel();
            client
                .send(ClientRequest {
                    request: Request::BlockDownload {
                        hash: block.hash(),
                        advertiser: Some(crate::test_support::connection_info(source)),
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
            let (mut connection, _client, mut inbound, _outbound, errors) =
                new_test_connection::<PanicAssertion>();
            Arc::make_mut(&mut connection.connection_info).early_block_relay = true;
            let advertiser = connection
                .connection_info
                .connected_addr
                .get_transient_addr()
                .map(|_| connection.connection_info.clone());
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
                                    advertiser.clone(),
                                    block.header.previous_block_hash,
                                ))
                                .await
                                .respond_error("duplicate or over-capacity download".into());
                            return;
                        }
                        check.respond_error("known header does not extend the best tip".into());
                    }
                    let request = inbound
                        .expect_request(Request::AdvertiseCompactBlock(
                            block.hash(),
                            advertiser.clone(),
                            block.header.previous_block_hash,
                        ))
                        .await;
                    if attempt == 1 {
                        request.respond_error("duplicate compact announcement".into());
                    } else {
                        request.respond(Response::Nil);
                    }
                };
                tokio::join!(receive, screen);
                assert!(
                    errors.try_get_error().is_none(),
                    "a rejected compact does not disconnect"
                );
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
        let (mut connection, _client, mut inbound, errors) =
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
        // The failed write starts shutdown without awaiting the stalled sink's close.
        send.await;
        assert!(delivered.await.unwrap().is_err());
        assert_eq!(quota.available_permits(), 1);
        assert!(errors.try_get_error().is_some());
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
