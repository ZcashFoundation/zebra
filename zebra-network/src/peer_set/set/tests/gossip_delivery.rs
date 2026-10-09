//! Completed and deferred mined-block inventory delivery.

use std::time::Duration;

use futures::FutureExt;
use tokio::time::timeout;
use tower::{Service, ServiceExt};

use super::*;
use crate::{constants::CURRENT_NETWORK_PROTOCOL_VERSION, Request, Response};

#[test]
fn all_peer_broadcast_waits_for_delivery_and_deduplicates_success() {
    let (runtime, _guard) = zebra_test::init_async();
    runtime.block_on(async {
        timeout(Duration::from_secs(10), async {
            let peers = PeerVersions {
                peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION],
            };
            let (discovered, mut handles) = peers.mock_peer_discovery();
            let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut set, _set_guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .build();
            set.ready().await.unwrap();
            let hash = block::Hash([1; 32]);
            let ordinary =
                tokio::spawn(set.route_sidecar_broadcast(Request::AdvertiseBlock(hash, None)));
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            assert_eq!(request.request, Request::AdvertiseBlock(hash, None));
            assert!(
                !ordinary.is_finished(),
                "delivery is not complete until the peer response"
            );
            request.tx.send(Ok(Response::Nil)).unwrap();
            ordinary.await.unwrap().unwrap();
            set.ready().await.unwrap();

            // Upgrade coverage without duplicating a successfully completed ordinary INV.
            set.broadcast_all(Request::AdvertiseBlockToAll(hash))
                .await
                .unwrap();
            assert!(handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .is_none());
            assert!(
                set.queued_broadcast_all.is_empty(),
                "ready peers must not also enter the busy snapshot"
            );
        })
        .await
        .unwrap();
    });
}

#[test]
fn consecutive_mined_inventories_survive_a_busy_peer() {
    let (runtime, _guard) = zebra_test::init_async();
    runtime.block_on(async {
        timeout(Duration::from_secs(10), async {
            let peers = PeerVersions {
                peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION],
            };
            let (discovered, mut handles) = peers.mock_peer_discovery();
            let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut set, _set_guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .build();
            let busy = set.ready().await.unwrap().call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            let first_hash = block::Hash([1; 32]);
            let second_hash = block::Hash([2; 32]);
            let first = tokio::spawn(set.broadcast_all(Request::AdvertiseBlockToAll(first_hash)));
            let second = tokio::spawn(set.broadcast_all(Request::AdvertiseBlockToAll(second_hash)));
            assert_eq!(
                set.queued_broadcast_all.len(),
                2,
                "a later block must not overwrite pending inventory"
            );
            tokio::task::yield_now().await;
            assert!(!first.is_finished());
            assert!(!second.is_finished());
            request.tx.send(Ok(Response::BlockHashes(vec![]))).unwrap();
            busy.await.unwrap();

            let _ = set.ready().now_or_never();
            tokio::task::yield_now().await;
            let delivered = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            assert_eq!(delivered.request, Request::AdvertiseBlockToAll(first_hash));
            assert!(
                !first.is_finished(),
                "queued sends must keep their response receivers alive"
            );
            delivered.tx.send(Ok(Response::Nil)).unwrap();
            first.await.unwrap().unwrap();

            let _ = set.ready().now_or_never();
            tokio::task::yield_now().await;
            let delivered = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            assert_eq!(delivered.request, Request::AdvertiseBlockToAll(second_hash));
            delivered.tx.send(Ok(Response::Nil)).unwrap();
            second.await.unwrap().unwrap();
            assert!(set.queued_broadcast_all.is_empty());
        })
        .await
        .unwrap();
    });
}

#[test]
fn canceled_queued_broadcast_does_not_leave_bookkeeping() {
    let (runtime, _guard) = zebra_test::init_async();
    runtime.block_on(async {
        timeout(Duration::from_secs(10), async {
            let peers = PeerVersions {
                peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION],
            };
            let (discovered, mut handles) = peers.mock_peer_discovery();
            let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut set, _set_guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .build();
            let busy = set.ready().await.unwrap().call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            drop(set.broadcast_all(Request::AdvertiseBlockToAll(block::Hash([1; 32]))));
            assert_eq!(set.queued_broadcast_all.len(), 1);
            set.broadcast_all_queued();
            assert!(set.queued_broadcast_all.is_empty());
            request.tx.send(Ok(Response::BlockHashes(vec![]))).unwrap();
            busy.await.unwrap();
        })
        .await
        .unwrap();
    });
}

#[test]
fn failed_delivery_is_not_reported_or_cached_as_success() {
    let (runtime, _guard) = zebra_test::init_async();
    runtime.block_on(async {
        timeout(Duration::from_secs(10), async {
            let peers = PeerVersions {
                peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION],
            };
            let (discovered, mut handles) = peers.mock_peer_discovery();
            let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut set, _set_guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .build();
            let hash = block::Hash([3; 32]);
            let mut previous = None;
            for (advert, succeed, expected) in [
                (Request::AdvertiseBlockToAll(hash), false, None),
                (Request::AdvertiseBlockToAll(hash), true, Some(hash)),
            ] {
                set.ready().await.unwrap();
                let delivery = tokio::spawn(set.broadcast_all(advert.clone()));
                let request = handles[0]
                    .try_to_receive_outbound_client_request()
                    .request()
                    .unwrap();
                assert_eq!(request.request, advert);
                assert!(set
                    .completed_block_adverts
                    .values()
                    .all(|completed| *completed.borrow() == previous));
                let response = if succeed {
                    Ok(Response::Nil)
                } else {
                    Err(crate::PeerError::ConnectionClosed.into())
                };
                request.tx.send(response).unwrap();
                assert_eq!(delivery.await.unwrap().is_ok(), succeed);
                assert!(set
                    .completed_block_adverts
                    .values()
                    .all(|completed| *completed.borrow() == expected));
                previous = expected;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
fn pending_inventory_queue_is_bounded_without_overwriting() {
    let (runtime, _guard) = zebra_test::init_async();
    runtime.block_on(async {
        timeout(Duration::from_secs(10), async {
            let peers = PeerVersions {
                peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION; 2],
            };
            let (discovered, mut handles) = peers.mock_peer_discovery();
            let (minimum, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut set, _set_guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .max_conns_per_ip(2)
                .build();
            set.ready().await.unwrap();
            assert_eq!(set.ready_services.len(), 2);
            let key = *set.ready_services.keys().next().unwrap();
            let mut service = set.take_ready_service(&key).unwrap();
            // The mock channel has one buffered slot plus one sender-reserved slot.
            // Fill both and leave requests buffered so polling cannot mark this peer ready.
            let mut busy = Vec::new();
            for _ in 0..2 {
                busy.push(service.ready().await.unwrap().call(Request::FindBlocks {
                    known_blocks: vec![],
                    stop: None,
                }));
            }
            set.push_unready(key, service);
            let mut receivers = Vec::new();
            for _ in 0..32 {
                receivers.push(
                    set.queue_broadcast_all_unready(&Request::AdvertiseBlockToAll(block::Hash(
                        [4; 32],
                    )))
                    .unwrap(),
                );
            }
            assert!(
                set.ready().now_or_never().is_none(),
                "full pending bookkeeping applies backpressure even with another ready peer"
            );
            assert_eq!(set.queued_broadcast_all.len(), 32);
            drop(receivers);
            set.broadcast_all_queued();
            assert!(set.queued_broadcast_all.is_empty());
            for response in busy {
                let request = handles
                    .iter_mut()
                    .find_map(|handle| handle.try_to_receive_outbound_client_request().request())
                    .unwrap();
                request.tx.send(Ok(Response::BlockHashes(vec![]))).unwrap();
                response.await.unwrap();
            }
            set.ready().await.unwrap();
        })
        .await
        .unwrap();
    });
}
