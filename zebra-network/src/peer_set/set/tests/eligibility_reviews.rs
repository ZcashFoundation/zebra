//! Consumer-visible eligibility and scheduling regressions from the auxiliary review.

use super::*;

#[test]
fn queued_block_ignores_banned_or_outdated_busy_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        for ban in [true, false] {
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
            let (mut peers, _guard) = PeerSetBuilder::new()
                .with_discover(discovery)
                .with_minimum_peer_version(minimum)
                .build();
            let (bans_tx, bans_rx) = watch::channel(crate::BanList::default());
            peers.bans_receiver = bans_rx;
            let _busy = make_serving_peer_busy(&mut peers, serving_addr).await;

            if ban {
                bans_tx.send_modify(|bans| {
                    bans.ban(serving_addr.ip());
                });
            } else {
                best_tip.send_best_tip_height(
                    NetworkUpgrade::Nu6_3.activation_height(&Network::Mainnet),
                );
            }
            let _download = timeout(Duration::from_secs(1), peers.ready())
                .await
                .expect("a usable fallback remains connected")
                .expect("fallback is ready")
                .call(block_request(2));
            assert_eq!(
                received_request(&mut fallback_handle),
                Some(block_request(2)),
                "a busy ineligible connection must not suppress a usable fallback (ban={ban})"
            );
        }
    });
}
