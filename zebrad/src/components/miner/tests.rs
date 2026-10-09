//! Controlled-channel tests for solver work changes.

use super::*;
use zebra_chain::serialization::ZcashDeserializeInto;

fn template() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vector is a valid block")
}

#[tokio::test(start_paused = true)]
async fn initial_template_wakes_solver_without_retry_delay() {
    let (sender, receiver) = watch::channel(None);
    let mut receiver = WatchReceiver::new(receiver);
    let start = tokio::time::Instant::now();
    let waiter = tokio::spawn(async move {
        wait_for_mining_template_change(&mut receiver).await;
        receiver.cloned_watch_data()
    });
    tokio::task::yield_now().await;
    sender.send(Some(template())).unwrap();
    assert!(waiter.await.unwrap().is_some());
    assert_eq!(tokio::time::Instant::now(), start);
}

#[tokio::test(start_paused = true)]
async fn parent_change_interrupts_refresh_and_retry_cooldowns() {
    for delay in [BLOCK_TEMPLATE_REFRESH_LIMIT, BLOCK_TEMPLATE_WAIT_TIME] {
        let (sender, mut receiver) = watch::channel(Some((block::Height(1), block::Hash([1; 32]))));
        let start = tokio::time::Instant::now();
        let waiter =
            tokio::spawn(async move { wait_for_mining_tip_change(&mut receiver, delay).await });
        tokio::task::yield_now().await;
        sender
            .send(Some((block::Height(2), block::Hash([2; 32]))))
            .unwrap();
        assert!(waiter.await.unwrap().unwrap());
        assert_eq!(tokio::time::Instant::now(), start);
    }
}

#[tokio::test(start_paused = true)]
async fn unchanged_parent_retains_refresh_rate_limit() {
    let (_sender, mut receiver) = watch::channel(None);
    let start = tokio::time::Instant::now();
    assert!(
        !wait_for_mining_tip_change(&mut receiver, BLOCK_TEMPLATE_REFRESH_LIMIT)
            .await
            .unwrap()
    );
    assert_eq!(
        tokio::time::Instant::now() - start,
        BLOCK_TEMPLATE_REFRESH_LIMIT
    );
}

#[tokio::test]
async fn solved_work_is_rechecked_after_the_last_solver_cancellation_check() {
    let block = template();
    let old_header = *block.header;
    let (template_sender, receiver) = watch::channel(Some(block));
    let receiver = WatchReceiver::new(receiver);
    let (tip_sender, mining_tip) =
        watch::channel(Some((block::Height(0), old_header.previous_block_hash)));
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true).is_ok());

    // Release a controlled solver result only after its parent has been replaced.
    let (solution_sender, solution_receiver) = tokio::sync::oneshot::channel();
    let submission = tokio::spawn(async move {
        solution_receiver.await.unwrap();
        cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true)
    });
    tip_sender
        .send(Some((block::Height(1), block::Hash([9; 32]))))
        .unwrap();
    solution_sender.send(()).unwrap();
    assert!(submission.await.unwrap().is_err());
    drop(template_sender);
}

#[test]
fn compatible_mempool_work_survives_but_replacement_and_withdrawal_cancel() {
    let old_block = template();
    let old_header = *old_block.header;
    let mut new_header = old_header;
    new_header.nonce[0] ^= 1;
    assert!(!should_replace_mining_template(
        Some(old_header),
        new_header,
        Some(true)
    ));
    assert!(should_replace_mining_template(
        Some(old_header),
        new_header,
        Some(false)
    ));
    assert!(should_replace_mining_template(None, new_header, Some(true)));

    let (sender, receiver) = watch::channel(Some(old_block.clone()));
    let receiver = WatchReceiver::new(receiver);
    let (tip_sender, mining_tip) =
        watch::channel(Some((block::Height(0), old_header.previous_block_hash)));
    sender.send(Some(old_block.clone())).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true).is_ok());
    let mut replacement = (*old_block).clone();
    replacement.header = Arc::new(new_header);
    sender.send(Some(Arc::new(replacement))).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true).is_err());
    sender.send(None).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true).is_err());
    sender.send(Some(old_block)).unwrap();
    drop(tip_sender);
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header, true).is_err());
}

#[test]
fn private_testnet_never_disables_mainnet_policy() {
    let mut config = Config::default();
    let testnet = Network::new_default_testnet();
    let regtest = Network::new_regtest(Default::default());
    assert!(!is_private_mining(&Network::Mainnet, &config));
    assert!(!is_private_mining(&testnet, &config));
    assert!(is_private_mining(&regtest, &config));
    config.internal_miner_private_testnet = true;
    assert!(!is_private_mining(&Network::Mainnet, &config));
    assert!(is_private_mining(&testnet, &config));
}

#[tokio::test]
async fn eligibility_loss_cancels_unchanged_work_and_returned_solution() {
    let block = template();
    let header = *block.header;
    let (_sender, receiver) = watch::channel(Some(block));
    let receiver = WatchReceiver::new(receiver);
    let (_sender, tip) = watch::channel(Some((block::Height(0), header.previous_block_hash)));
    assert!(cancel_if_mining_template_changed(&receiver, &tip, header, true).is_ok());
    assert!(cancel_if_mining_template_changed(&receiver, &tip, header, false).is_err());
    assert!(cancel_if_mining_template_changed(&receiver, &tip, header, true).is_ok());

    let (release, solved) = tokio::sync::oneshot::channel();
    let submission = tokio::spawn(async move {
        solved.await.unwrap();
        cancel_if_mining_template_changed(&receiver, &tip, header, false)
    });
    release.send(()).unwrap();
    assert!(submission.await.unwrap().is_err());
}

#[test]
fn response_classification_retains_uncertain_local_identity() {
    let rejected: Result<_, &'static str> = Ok(SubmitBlockErrorResponse::Rejected.into());
    assert!(submission_was_rejected(&rejected));
    for response in [
        Ok(SubmitBlockResponse::Accepted),
        Ok(SubmitBlockErrorResponse::Inconclusive.into()),
        Ok(SubmitBlockErrorResponse::DuplicateInconclusive.into()),
        Ok(SubmitBlockErrorResponse::Duplicate.into()),
        Err("transport error after admission"),
    ] {
        assert!(!submission_was_rejected(&response));
    }
}

#[test]
fn pending_parent_requires_commit_and_a_foreign_public_parent() {
    let hash = block::Hash([1; 32]);
    let previous = block::Hash([2; 32]);
    let submitted = SubmittedBlock {
        hash,
        height: block::Height(2),
    };
    for private in [false, true] {
        assert!(!submitted.permits_work(hash, None, private));
        assert!(!submitted.permits_work(hash, Some((block::Height(1), previous)), private));
        assert!(!submitted.permits_work(previous, Some((block::Height(1), previous)), private));
    }
    assert!(!submitted.permits_work(hash, Some((block::Height(2), hash)), false));
    assert!(submitted.permits_work(hash, Some((block::Height(2), hash)), true));
    let external = block::Hash([3; 32]);
    assert!(submitted.permits_work(external, Some((block::Height(2), external)), false));
    assert!(!submitted.permits_work(hash, Some((block::Height(2), external)), true));
}

#[tokio::test]
async fn regtest_native_candidate_uses_null_solution_and_honors_cancellation() {
    let network = Network::new_regtest(Default::default());
    let template = template();
    let blocks = mine_a_block(3, template.clone(), network.clone(), || Ok(()))
        .await
        .unwrap();
    let block = blocks.into_iter().next().unwrap();
    assert_eq!(block.header.solution, Solution::Regtest([0; 36]));
    assert_eq!(block.header.nonce[0], 3);
    assert_eq!(
        block.header.previous_block_hash,
        template.header.previous_block_hash
    );
    assert_eq!(block.transactions, template.transactions);
    assert!(mine_a_block(0, template, network, || Err(SolverCancelled))
        .await
        .is_err());
}

#[tokio::test]
async fn public_policy_uses_current_sync_and_actual_peer_response_freshness() {
    use zebra_chain::{chain_sync_status::MockSyncStatus, chain_tip::mock::MockChainTip};
    use zebra_network::{
        address_book_peers::MockAddressBookPeers, types::MetaAddr, PeerSocketAddr,
    };
    use zebra_test::mock_service::MockService;

    #[derive(Clone)]
    struct Peers(watch::Sender<MockAddressBookPeers>);
    impl AddressBookPeers for Peers {
        fn recently_live_peers(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<MetaAddr> {
            self.0.borrow().recently_live_peers(now)
        }
        fn has_recently_live_peers(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
            self.0.borrow().has_recently_live_peers(now)
        }
        fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
            self.0.send_modify(|peers| {
                peers.add_peer(peer);
            });
            true
        }
    }

    let (sender, _receiver) = watch::channel(MockAddressBookPeers::default());
    let mut peers = Peers(sender);
    let addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    peers.add_peer(addr);
    let mut sync = MockSyncStatus::default();
    sync.set_is_close_to_tip(true);
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(3_000_000));
    tip_sender.send_best_tip_hash(block::Hash([3; 32]));
    tip_sender.send_best_tip_block_time(chrono::Utc::now());
    let (_logs, logs) = watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Network::Mainnet,
        Config::default(),
        Default::default(),
        "0.0.1",
        "miner policy test",
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        block::Height(0),
        sync.clone(),
        tip,
        peers.clone(),
        logs,
        None,
    );
    assert!(rpc.public_mining_is_eligible());
    sync.set_is_close_to_tip(false);
    assert!(!rpc.public_mining_is_eligible());
    sync.set_is_close_to_tip(true);
    assert!(rpc.public_mining_is_eligible());
    // A still-present Responded entry is not proof of current liveness.
    let stale = MetaAddr::new_responded(addr, None).into_new_meta_addr(
        std::time::Instant::now(),
        (chrono::Utc::now() - chrono::Duration::days(1))
            .try_into()
            .unwrap(),
    );
    peers.0.send_replace(MockAddressBookPeers::new(vec![stale]));
    assert!(!rpc.public_mining_is_eligible());
    peers.add_peer(addr);
    assert!(rpc.public_mining_is_eligible());
    peers.0.send_replace(MockAddressBookPeers::default());
    assert!(!rpc.public_mining_is_eligible());
    assert!(
        rpc.private_mining_template(None).await.is_err(),
        "Mainnet cannot override policy"
    );
    queue.abort();
}

#[tokio::test]
async fn native_regtest_submission_does_not_extend_an_inconclusive_uncommitted_parent() {
    use zebra_chain::{chain_sync_status::MockSyncStatus, chain_tip::mock::MockChainTip};
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_test::mock_service::MockService;

    let mut read_state: MockService<_, _, _, zebra_state::BoxError> =
        MockService::build().for_unit_tests();
    let mut verifier: MockService<_, _, _, zebra_consensus::BoxError> =
        MockService::build().for_unit_tests();
    let template = template();
    let parent = template.header.previous_block_hash;
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(0));
    tip_sender.send_best_tip_hash(parent);
    let (_logs, logs) = watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Network::new_regtest(Default::default()),
        Config::default(),
        Default::default(),
        "0.0.1",
        "native miner submission test",
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        read_state.clone(),
        verifier.clone(),
        block::Height(0),
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        logs,
        None,
    );
    let (templates, receiver) = watch::channel(Some(template));
    let (mining_tip, changes) = watch::channel(Some((block::Height(0), parent)));
    let solver = tokio::spawn(run_mining_solver(
        0,
        Config::default(),
        WatchReceiver::new(receiver),
        rpc,
    ));
    read_state
        .expect_request(zebra_state::ReadRequest::MiningTipChange)
        .await
        .respond(zebra_state::ReadResponse::MiningTipChange(
            zebra_state::MiningTipChange { receiver: changes },
        ));
    let first = verifier.expect_request_that(|_| true).await;
    let solved = first.request().block();
    assert_eq!(solved.header.solution, Solution::Regtest([0; 36]));
    let hash = solved.hash();
    let mut next: Block = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    Arc::make_mut(&mut next.header).previous_block_hash = hash;
    mining_tip.send(Some((block::Height(1), hash))).unwrap();
    templates.send(Some(Arc::new(next))).unwrap();

    // Full verification outlives submitblock's response deadline. The solver must retain
    // the submitted identity and refuse the speculative parent after Inconclusive.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(BLOCK_TEMPLATE_REFRESH_LIMIT).await;
    verifier.expect_no_requests().await;
    tip_sender.send_best_tip_height(block::Height(1));
    tip_sender.send_best_tip_hash(hash);
    first.respond(hash);
    tokio::time::advance(BLOCK_TEMPLATE_REFRESH_LIMIT).await;
    tokio::time::resume();
    let second = verifier.expect_request_that(|_| true).await;
    assert_eq!(second.request().block().header.previous_block_hash, hash);
    let second_hash = second.request().block().hash();
    second.respond(second_hash);
    drop(templates);
    solver.abort();
    assert!(solver.await.unwrap_err().is_cancelled());
    queue.abort();
}
