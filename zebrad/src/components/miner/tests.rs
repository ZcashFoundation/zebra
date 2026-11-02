//! Internal miner notification and candidate-retention tests.

use futures::FutureExt;
use zebra_chain::serialization::{DateTime32, ZcashDeserializeInto};

use super::*;

#[tokio::test(start_paused = true)]
async fn idle_solver_wakes_when_template_arrives() {
    let block = zebra_chain::block::genesis::regtest_genesis_block();
    let (sender, receiver) = watch::channel(None);
    let mut receiver = WatchReceiver::new(receiver);
    receiver.mark_as_seen();
    assert!(receiver.cloned_watch_data().is_none());

    let start = tokio::time::Instant::now();
    let send_work = tokio::spawn(async move {
        sleep(Duration::from_secs(1)).await;
        sender
            .send(Some(block))
            .expect("the idle solver still receives work");
        sender
    });
    wait_for_mining_template_change(&mut receiver).await;
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    assert!(receiver.cloned_watch_data().is_some());
    let sender = send_work.await.expect("the sender task does not panic");

    // An update between the empty read and the wait must not be lost.
    sender.send_replace(None);
    receiver.mark_as_seen();
    assert!(receiver.cloned_watch_data().is_none());
    sender.send_replace(Some(zebra_chain::block::genesis::regtest_genesis_block()));
    assert!(wait_for_mining_template_change(&mut receiver)
        .now_or_never()
        .is_some());

    let start = tokio::time::Instant::now();
    wait_for_mining_template_change(&mut receiver).await;
    assert_eq!(start.elapsed(), BLOCK_TEMPLATE_WAIT_TIME);
    drop(sender);
    assert!(wait_for_mining_template_change(&mut receiver)
        .now_or_never()
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn parked_solver_rechecks_parent_without_new_work() {
    let (sender, receiver) =
        watch::channel(Some(zebra_chain::block::genesis::regtest_genesis_block()));
    let mut receiver = WatchReceiver::new(receiver);
    receiver.mark_as_seen();

    // Retained work is not republished when its parent commits, so the wait must end anyway.
    let start = tokio::time::Instant::now();
    wait_for_committed_parent(&mut receiver)
        .await
        .expect("the template sender is still open");
    assert_eq!(start.elapsed(), BLOCK_TEMPLATE_REFRESH_LIMIT);

    drop(sender);
    assert!(wait_for_committed_parent(&mut receiver).await.is_err());
}

#[test]
fn submitold_retains_only_eligible_mining_candidates() {
    let original: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("the block fixture has valid serialization");
    let second: Block = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("the block fixture has valid serialization");
    let mut refresh = (*original).clone();
    Arc::make_mut(&mut refresh.header).merkle_root = second.header.merkle_root;
    let time = original.header.time;
    let valid_times = time..=time;

    let (sender, receiver) = watch::channel(Some(original.clone()));
    assert!(!sender.send_if_modified(|current| {
        update_mining_template(current, refresh.clone(), Some(true), valid_times.clone())
    }));
    assert!(!receiver.has_changed().expect("the sender is alive"));
    assert!(Arc::ptr_eq(receiver.borrow().as_ref().unwrap(), &original));

    for submit_old in [None, Some(false)] {
        let mut current = Some(original.clone());
        assert!(update_mining_template(
            &mut current,
            refresh.clone(),
            submit_old,
            valid_times.clone(),
        ));
        assert_eq!(current.unwrap().header, refresh.header);
    }

    let mut other_parent = refresh.clone();
    Arc::make_mut(&mut other_parent.header).previous_block_hash = second.header.previous_block_hash;
    let mut other_height = refresh.clone();
    other_height.transactions = second.transactions;
    let mut other_difficulty = refresh.clone();
    Arc::make_mut(&mut other_difficulty.header).difficulty_threshold =
        zebra_chain::work::difficulty::CompactDifficulty::from_bytes_in_display_order(&[
            0x20, 0x07, 0xff, 0xff,
        ])
        .expect("the test difficulty is representable");
    for replacement in [other_parent, other_height, other_difficulty] {
        assert!(update_mining_template(
            &mut Some(original.clone()),
            replacement,
            Some(true),
            valid_times.clone(),
        ));
    }
    for invalid_time in [DateTime32::MIN, DateTime32::MAX] {
        assert!(update_mining_template(
            &mut Some(original.clone()),
            refresh.clone(),
            Some(true),
            invalid_time.into()..=invalid_time.into(),
        ));
    }
    assert!(update_mining_template(
        &mut None,
        refresh,
        Some(true),
        valid_times
    ));
}

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
