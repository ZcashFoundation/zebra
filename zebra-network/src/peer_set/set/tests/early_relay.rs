//! Service-bit negotiation regressions through real handshakes, adverts, and getdata.

use std::{collections::HashSet, sync::Arc};

use tokio_util::codec::Framed;
use zebra_chain::chain_tip::NoChainTip;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    peer::{Handshake, HandshakeRequest},
    protocol::{external::Codec, internal::InventoryResponse},
    types::Nonce,
    Config,
};

use super::*;

/// Bit 24 is used for a different capability, not early block relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bit24_excludes_early_adverts_and_serving_but_bit25_opts_in() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(30), async {
        let block: Arc<block::Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .as_slice()
            .zcash_deserialize_into()
            .unwrap();
        for (remote_bit, early_relay) in [(1 << 24, false), (1 << 25, true)] {
            let mut inbound: MockService<Request, Response, PanicAssertion> =
                MockService::build().finish();
            let network = Network::Mainnet;
            let handshake = Handshake::builder()
                .with_config(Config {
                    network: network.clone(),
                    experimental_early_block_relay: true,
                    ..Config::default()
                })
                .with_inbound_service(inbound.clone())
                .with_advertised_services(PeerServices::NODE_NETWORK)
                .with_user_agent("/same-user-agent/".to_string())
                .finish()
                .expect("the handshake has its mandatory config and inbound service");
            let (local, remote) = tokio::io::duplex(65536);
            let mut remote = Framed::new(remote, Codec::builder().for_network(&network).finish());
            let addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
            let (client, ()) = tokio::join!(
                handshake.oneshot(HandshakeRequest {
                    data_stream: local,
                    connected_addr: ConnectedAddr::new_inbound_direct(addr),
                    connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
                }),
                async {
                    let Message::Version(mut version) = remote.next().await.unwrap().unwrap()
                    else {
                        panic!("the handshake starts with a version message");
                    };
                    assert_eq!(
                        version.services.bits(),
                        PeerServices::NODE_NETWORK.bits() | (1 << 25)
                    );
                    // Retain the unknown bit so the collision is tested on the actual wire.
                    version.services =
                        PeerServices::NODE_NETWORK | PeerServices::from_bits_retain(remote_bit);
                    version.nonce = Nonce::default();
                    remote.send(Message::Version(version)).await.unwrap();
                    remote.send(Message::Verack).await.unwrap();
                    assert_eq!(remote.next().await.unwrap().unwrap(), Message::Verack);
                },
            );
            let client = client.expect("both service sets permit ordinary full-node connections");
            assert_eq!(client.connection_info.early_block_relay, early_relay);
            remote.codec_mut().reconfigure_full_body_len();
            remote
                .codec_mut()
                .reconfigure_early_block_relay(early_relay);
            if early_relay {
                assert_eq!(
                    remote.next().await.unwrap().unwrap(),
                    Message::SendCompact {
                        high_bandwidth: false,
                        version: 1
                    },
                );
            }

            let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
                .chain(stream::pending());
            let (mut peer_set, _guard) = PeerSetBuilder::new()
                .with_discover(discover)
                .with_minimum_peer_version(MinimumPeerVersion::new(NoChainTip, &network))
                .with_block_gossip_peer_ips(vec![addr.ip()])
                .build();
            peer_set.ready().await.unwrap();
            if early_relay {
                assert_eq!(
                    remote.next().await.unwrap().unwrap(),
                    Message::SendCompact {
                        high_bandwidth: true,
                        version: 1
                    },
                );
            }
            let mut advertiser = peer_set.block_advertiser();
            let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
            let hash = block.hash();
            let result = advertiser
                .ready()
                .await
                .unwrap()
                .call(Request::AdvertiseEarlyBlock(hash))
                .await;
            if early_relay {
                assert_eq!(result.unwrap(), Response::Nil);
                assert_eq!(
                    remote.next().await.unwrap().unwrap(),
                    Message::Inv(vec![InventoryHash::Block(hash)])
                );
            } else {
                assert!(
                    result.is_err(),
                    "bit 24 must not authorize early adverts, even to sidecars"
                );
                assert!(remote.next().now_or_never().is_none());
            }

            // Before commit, only the negotiated request may read the early serving cache.
            remote
                .send(Message::GetData(vec![hash.into()]))
                .await
                .unwrap();
            let hashes = HashSet::from([hash]);
            let request = if early_relay {
                Request::EarlyBlocksByHash(hashes)
            } else {
                Request::BlocksByHash(hashes)
            };
            let (response, expected_message) = if early_relay {
                (
                    Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))]),
                    Message::Block(block.clone()),
                )
            } else {
                (
                    Response::Blocks(vec![InventoryResponse::Missing(hash)]),
                    Message::NotFound(vec![hash.into()]),
                )
            };
            inbound
                .expect_request(request.clone())
                .await
                .respond(response);
            assert_eq!(remote.next().await.unwrap().unwrap(), expected_message);

            // Committed inventory and block serving remain available to either peer.
            assert_eq!(
                advertiser
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::AdvertiseBlockToAll(hash))
                    .await
                    .unwrap(),
                Response::Nil
            );
            assert_eq!(
                remote.next().await.unwrap().unwrap(),
                Message::Inv(vec![InventoryHash::Block(hash)])
            );
            remote
                .send(Message::GetData(vec![hash.into()]))
                .await
                .unwrap();
            inbound
                .expect_request(request)
                .await
                .respond(Response::Blocks(vec![InventoryResponse::Available((
                    block.clone(),
                    None,
                ))]));
            assert_eq!(
                remote.next().await.unwrap().unwrap(),
                Message::Block(block.clone())
            );
        }
    })
    .await
    .expect("service-bit negotiation and relay must complete within the test deadline");
}

/// Source credit requires a committed block, and backpressured demotion cannot delay replacement.
#[tokio::test]
async fn compact_receive_sources_prefer_committed_first_announcers_and_release_disconnections() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(10), async {
        let mut discovered = Vec::new();
        let mut connections = Vec::new();
        let mut messages = Vec::new();
        let mut preferences = Vec::new();
        let mut harnesses = Vec::new();
        let addrs: Vec<PeerSocketAddr> = (1..=5)
            .map(|ip| SocketAddr::from(([127, 0, 0, ip], 8233)).into())
            .collect();
        for addr in &addrs {
            let (client, harness, connection, wire) = real_advert_peer_with_authorization(
                CURRENT_NETWORK_PROTOCOL_VERSION,
                true,
                MinimumPeerVersion::new(NoChainTip, &Network::Mainnet),
                watch::channel(crate::BanList::default()).1,
            );
            preferences.push(client.compact_receive_tx.clone());
            discovered.push(Ok::<_, BoxError>(Change::Insert(*addr, client.into())));
            connections.push(connection);
            messages.push(wire);
            harnesses.push(harness);
        }
        let (minimum, tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (mut peer_set, mut guard) = PeerSetBuilder::new()
            .with_discover(stream::iter(discovered).chain(stream::pending()))
            .with_minimum_peer_version(minimum)
            .build();
        peer_set.ready().await.unwrap();
        let mut tasks: Vec<_> = connections
            .into_iter()
            .map(|connection| Some(tokio::spawn(connection)))
            .collect();
        for (index, wire) in messages.iter_mut().enumerate() {
            assert_eq!(
                wire.next().await,
                Some(Message::SendCompact {
                    high_bandwidth: index < 3,
                    version: 1,
                })
            );
        }
        let collector = guard.inventory_sender().as_ref().unwrap().clone();
        for (source, hash, expected) in [
            (4, 10, [true, true, false, false, true]),
            // A later announcement of the same block cannot win the source slot.
            (3, 10, [true, true, false, false, true]),
            (1, 11, [true, true, false, false, true]),
            (3, 12, [false, true, false, true, true]),
        ] {
            let previous: Vec<_> = preferences
                .iter()
                .map(|preference| preference.borrow().is_some())
                .collect();
            crate::peer::register_inventory_status(
                Ok(Message::Inv(vec![InventoryHash::Block(block::Hash(
                    [hash; 32],
                ))])),
                ConnectedAddr::new_inbound_direct(addrs[source]),
                true,
                collector.clone(),
            )
            .await
            .unwrap();
            peer_set.ready().await.unwrap();
            assert_eq!(
                preferences
                    .iter()
                    .map(|preference| preference.borrow().is_some())
                    .collect::<Vec<_>>(),
                previous,
                "unvalidated inventory cannot change source selection",
            );
            tip.send_best_tip_hash(block::Hash([hash; 32]));
            peer_set.ready().await.unwrap();
            assert_eq!(
                preferences
                    .iter()
                    .map(|preference| preference.borrow().is_some())
                    .collect::<Vec<_>>(),
                expected
            );
            let replacement = match (source, hash) {
                (4, 10) => Some((2, 4)),
                (3, 12) => Some((0, 3)),
                _ => None,
            };
            if let Some((demoted, promoted)) = replacement {
                // The new source proceeds even if the demoted peer has not read its notification.
                assert_eq!(
                    messages[promoted].next().await,
                    Some(Message::SendCompact {
                        high_bandwidth: true,
                        version: 1,
                    })
                );
                assert_eq!(
                    messages[demoted].next().await,
                    Some(Message::SendCompact {
                        high_bandwidth: false,
                        version: 1,
                    })
                );
            }
        }
        let disconnected = tasks[4].take().unwrap();
        disconnected.abort();
        assert!(disconnected.await.unwrap_err().is_cancelled());
        peer_set.remove(&addrs[4]);
        peer_set.ready().await.unwrap();
        assert_eq!(
            messages[0].next().await,
            Some(Message::SendCompact {
                high_bandwidth: true,
                version: 1,
            })
        );
        assert_eq!(
            preferences
                .iter()
                .filter(|preference| preference.borrow().is_some())
                .count(),
            3
        );
        for task in tasks.into_iter().flatten() {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    })
    .await
    .expect("source selection and replacement complete within the test deadline");
}
