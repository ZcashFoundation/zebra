//! Internal miner eligibility and notification-race regressions.

use zebra_chain::serialization::ZcashDeserializeInto;

use super::*;

#[test]
fn solver_observes_template_published_after_change_check() {
    let original: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("the original block fixture has valid serialization");
    let replacement: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("the replacement block fixture has valid serialization");
    let (sender, receiver) = watch::channel(Some(original.clone()));
    let mut receiver = WatchReceiver::new(receiver);
    receiver.mark_as_seen();
    let observed_change = receiver.has_changed().expect("the sender is alive");

    sender.send_replace(Some(replacement));
    if !mining_template_changed(&mut receiver, observed_change, &original.header) {
        let observed_change = receiver.has_changed().expect("the sender is alive");
        assert!(
            mining_template_changed(&mut receiver, observed_change, &original.header),
            "new work must cancel this solve without waiting for another publication"
        );
    }
}

/// Public mining stops at its own accepted tip; rejection and rollback still permit work.
#[tokio::test]
async fn internal_miner_does_not_extend_its_accepted_public_tip() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        chain_tip::NoChainTip,
        parameters::{testnet::ParametersBuilder, Network},
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    let first: Arc<Block> = zebra_test::vectors::BLOCK_TESTNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let child: Arc<Block> = zebra_test::vectors::BLOCK_TESTNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let first_hash = first.hash();
    let child_hash = child.hash();
    let private_testnet = ParametersBuilder::default()
        .with_network_name("PrivateMining")
        .unwrap()
        .to_network()
        .unwrap();
    for (network, public) in [
        (Network::Mainnet, true),
        (Network::new_default_testnet(), true),
        (Network::new_regtest(Default::default()), false),
        (private_testnet, false),
    ] {
        let initial_tip = (block::Height(0), first.header.previous_block_hash);
        let (tip, tip_receiver) = watch::channel(initial_tip);
        let read_tip = tip_receiver.clone();
        let read_state = tower::service_fn(move |request| {
            assert!(matches!(request, zebra_state::ReadRequest::Tip));
            std::future::ready(Ok::<_, zebra_state::BoxError>(
                zebra_state::ReadResponse::Tip(Some(*read_tip.borrow())),
            ))
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let verified_calls = calls.clone();
        let committed_tip = tip.clone();
        let verifier = tower::service_fn(move |request: zebra_consensus::Request| {
            let block = request.block();
            if verified_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return std::future::ready(Err(zebra_consensus::BoxError::from(
                    "pre-admission rejection",
                )));
            }
            let hash = block.hash();
            committed_tip.send_replace((block.coinbase_height().unwrap(), hash));
            std::future::ready(Ok(hash))
        });
        let mempool: MockService<_, _, _, zebra_node_services::BoxError> =
            MockService::build().for_unit_tests();
        let state: MockService<_, _, _, zebra_state::BoxError> =
            MockService::build().for_unit_tests();
        let mut sync_status = MockSyncStatus::default();
        sync_status.set_is_close_to_tip(true);
        let mut peers = MockAddressBookPeers::default();
        peers.add_peer(
            "127.0.0.1:8233"
                .parse::<std::net::SocketAddr>()
                .unwrap()
                .into(),
        );
        let (_logs, log_receiver) = watch::channel(None);
        let (mined_blocks, _announcements) = tokio::sync::mpsc::channel(4);
        let (rpc, tx_queue) = RpcImpl::new(
            network.clone(),
            Default::default(),
            Default::default(),
            "0.0.1",
            "miner policy test",
            mempool,
            state,
            read_state,
            verifier,
            sync_status,
            NoChainTip,
            peers,
            log_receiver,
            Some(mined_blocks),
        );
        assert!(rpc.internal_mining_is_enabled());
        let mut last_accepted = None;
        timeout(Duration::from_secs(5), async {
            assert!(!matches!(
                submit_mined_block(&first, &rpc, &mut last_accepted).await,
                Ok(Some((_, SubmitBlockResponse::Accepted)))
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(last_accepted.is_none());
            assert_eq!(*tip_receiver.borrow(), initial_tip);
            assert_eq!(
                submit_mined_block(&first, &rpc, &mut last_accepted)
                    .await
                    .unwrap(),
                Some((first_hash, SubmitBlockResponse::Accepted)),
            );
            let extension = submit_mined_block(&child, &rpc, &mut last_accepted)
                .await
                .unwrap();
            if public {
                assert!(
                    extension.is_none(),
                    "public mining must not extend its own accepted tip"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 2);
            } else {
                assert_eq!(extension, Some((child_hash, SubmitBlockResponse::Accepted)));
                assert_eq!(calls.load(Ordering::SeqCst), 3);
            }

            tip.send_replace(initial_tip);
            assert_eq!(
                submit_mined_block(&first, &rpc, &mut last_accepted)
                    .await
                    .unwrap(),
                Some((first_hash, SubmitBlockResponse::Accepted)),
                "a lower committed parent must remain eligible after rollback",
            );
        })
        .await
        .expect("mining policy checks use bounded service calls");
        tx_queue.abort();
    }
}
