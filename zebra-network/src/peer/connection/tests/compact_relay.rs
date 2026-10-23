//! Compact recovery uses the normal bounded downloader and falls back without scoring collisions.

use super::new_test_connection;
use crate::{
    compact::{BlockTransactions, CompactBlock},
    peer::ClientRequest,
    protocol::external::Message,
    InventoryResponse, Request, Response,
};
use futures::{
    channel::{mpsc, oneshot},
    SinkExt, StreamExt,
};
use std::{sync::Arc, time::Duration};
use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
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
        for failure in ["none", "missing", "timeout", "commitments"] {
            let (mut connection, mut client, mut inbound, mut outbound, errors) =
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
            let compact = Arc::new(CompactBlock::from_block(&block, 7).unwrap());
            peer.send(Ok(Message::CompactBlock(compact.clone())))
                .await
                .unwrap();
            inbound
                .expect_request(Request::AdvertiseBlock(block.hash(), Some(source)))
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
                peer.send(Ok(Message::Block(block.clone()))).await.unwrap();
            }
            assert_eq!(
                rx.await.unwrap().unwrap(),
                Response::EarlyBlocks(vec![InventoryResponse::Available((
                    block.clone(),
                    Some(source)
                ))])
            );
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
                allow_compact: true,
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
