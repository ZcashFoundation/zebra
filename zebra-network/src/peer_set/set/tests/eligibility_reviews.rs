//! Consumer-visible eligibility and scheduling regressions from the auxiliary review.

use futures::FutureExt as _;

use super::*;

#[test]
fn queued_block_ignores_banned_or_outdated_busy_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        for (ban, fallback_missing) in [(true, false), (false, false), (true, true), (false, true)]
        {
            let serving_addr: PeerSocketAddr = "127.0.0.1:1".parse().unwrap();
            let fallback_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
            let (serving, _serving_handle) = ClientTestHarness::build()
                .with_version(Version::min_remote_for_height(&Network::Mainnet, None))
                .with_services(PeerServices::NODE_NETWORK)
                .finish();
            let (fallback, mut fallback_handle) = ClientTestHarness::build()
                .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
                .with_services(PeerServices::empty())
                .finish();
            let discovery = stream::iter([
                Ok::<_, BoxError>(Change::Insert(serving_addr, serving.into())),
                Ok(Change::Insert(fallback_addr, fallback.into())),
            ])
            .chain(stream::pending());
            let (minimum, best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (mut peers, mut peer_guard) = PeerSetBuilder::new()
                .with_discover(discovery)
                .with_minimum_peer_version(minimum)
                .build();
            let (bans_tx, bans_rx) = watch::channel(crate::BanList::default());
            peers.bans_receiver = bans_rx;
            let _busy = make_serving_peer_busy(&mut peers, serving_addr).await;
            if fallback_missing {
                send_inventory(
                    &mut peer_guard,
                    InventoryStatus::new_missing(
                        InventoryHash::Block(block::Hash([2; 32])),
                        fallback_addr,
                    ),
                );
            }
            let download = peers.ready().await.unwrap().call(block_request(2));
            assert_eq!(peers.queued_inv_requests.len(), 1);

            if ban {
                bans_tx.send_modify(|bans| {
                    bans.ban(serving_addr.ip());
                });
            } else {
                best_tip.send_best_tip_height(
                    NetworkUpgrade::Nu6_3.activation_height(&Network::Mainnet),
                );
            }
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, peers.ready())
                .await
                .expect("eligibility changes must reconsider the existing queue promptly")
                .expect("fallback remains ready");
            assert!(peers.queued_inv_requests.is_empty());
            if fallback_missing {
                assert_not_found_registry(
                    download
                        .now_or_never()
                        .expect("no eligible candidate must refuse the queued request immediately"),
                );
                assert_eq!(received_request(&mut fallback_handle), None);
            } else {
                assert_eq!(
                    received_request(&mut fallback_handle),
                    Some(block_request(2)),
                    "removing a busy candidate must reroute the queued request (ban={ban})"
                );
            }
        }
    });
}
