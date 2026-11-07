//! A candidate read can observe a publication whose notification has not been consumed yet.

use zebra_test::mock_service::MockService;

use super::*;

#[tokio::test]
async fn a_publication_during_the_candidate_read_does_not_withdraw_live_work() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let network = Network::new_regtest(
            zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let genesis = zebra_chain::block::genesis::regtest_genesis_block();
        let genesis_hash = genesis.hash();
        let (state, read_state, tip, _tip_changes) =
            zebra_state::populated_state([genesis], &network).await;
        let miner = |kind| {
            MinerParams::from(
                Address::decode(&network, default_miner_address(network.kind(), &kind)).unwrap(),
            )
        };
        // Prepare a parent independently: the updater's first candidate read will be held before
        // it can publish any work for genesis.
        let info = fetch_chain_info(read_state.clone()).await.unwrap();
        let initial = BlockTemplateResponse::from_mempool(
            &network,
            &CoinbaseCache::default(),
            &miner(MinerAddressType::Transparent),
            &info,
            LongPollInput::new(
                info.tip_height,
                info.tip_hash,
                info.max_time,
                std::iter::empty(),
            )
            .generate_id(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let candidate = Arc::new(
            proposal_block_from_template(&initial, BlockTemplateTimeSource::CurTime, &network)
                .unwrap(),
        );
        let candidate_hash = candidate.hash();
        let ReadResponse::MiningCandidateChanges(mut candidates) = read_state
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("state provides candidate subscriptions");
        };
        let candidate_read_started = Arc::new(tokio::sync::Notify::new());
        let (release_candidate_read, candidate_read_released) = watch::channel(false);
        let (proof_started, proof_observed) = watch::channel(false);
        let observed_state = tower::service_fn({
            let read_state = read_state.clone();
            let candidate_read_started = candidate_read_started.clone();
            move |request| {
                let read_state = read_state.clone();
                let candidate_read_started = candidate_read_started.clone();
                let mut candidate_read_released = candidate_read_released.clone();
                let mut proof_observed = proof_observed.clone();
                async move {
                    if matches!(request, ReadRequest::MiningCandidate) {
                        candidate_read_started.notify_one();
                        candidate_read_released
                            .wait_for(|released| *released)
                            .await
                            .unwrap();
                    }
                    // The faulty updater checks commitment before clearing the template. Hold that
                    // check until speculation owns the permit, so a redundant rebuild cannot hide
                    // the withdrawal before the test observes it.
                    if matches!(request, ReadRequest::Tip) {
                        proof_observed.wait_for(|started| *started).await.unwrap();
                    }
                    read_state.oneshot(request).await
                }
            }
        });
        let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> = MockService::build()
            .with_max_request_delay(Duration::from_secs(10))
            .for_unit_tests();
        let coinbases = CoinbaseCache::default();
        let cache = TemplateCache::default();
        let (_events, events) = broadcast::channel(1);
        let verifier = tower::service_fn(|request: VerifyRequest| async move {
            Ok::<_, zebra_consensus::BoxError>(request.block().hash())
        });
        let mut updater = tokio::task::JoinSet::new();
        updater.spawn(run(
            network.clone(),
            miner(MinerAddressType::Sapling),
            coinbases.clone(),
            cache.clone(),
            mempool.clone(),
            events,
            observed_state,
            tip.clone(),
            MockSyncStatus::default(),
            verifier,
        ));
        candidate_read_started.notified().await;
        let zebra_state::Response::MiningCandidate(Some(_candidate_guard)) = state
            .clone()
            .oneshot(zebra_state::Request::MiningCandidate(candidate))
            .await
            .unwrap()
        else {
            panic!("the test and updater subscriptions enable candidate publication");
        };
        candidates.changed().await;
        assert!(candidates.is_current(candidate_hash));
        release_candidate_read.send_replace(true);
        let snapshot = mempool
            .expect_request(mempool::Request::FullTransactions)
            .await;
        let early = cache
            .template_for_tip(candidate_hash, &network, DateTime32::now())
            .unwrap();
        assert!(early.transactions.is_empty());
        let mut publications = cache.subscribe();
        let entries = coinbases.entries.clone();
        let (locked, lock_acquired) = tokio::sync::oneshot::channel();
        let (release_proof, proof_released) = tokio::sync::oneshot::channel();
        let lock_task = tokio::task::spawn_blocking(move || {
            let _guard = entries.lock().unwrap();
            locked.send(()).unwrap();
            let _ = proof_released.blocking_recv();
        });
        lock_acquired.await.unwrap();
        let survivor = VerifiedUnminedTx::new(
            Arc::new(Transaction::test_v1(
                vec![],
                vec![],
                zebra_chain::transaction::LockTime::Height(Height(1)),
            ))
            .into(),
            10_000.try_into().unwrap(),
            0,
            0,
            Arc::new(vec![]),
        )
        .unwrap();
        snapshot.respond(mempool::Response::FullTransactions {
            transactions: vec![survivor],
            transaction_dependencies: Default::default(),
            last_seen_tip_hash: genesis_hash,
        });
        while coinbases.build.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
        proof_started.send_replace(true);
        // A withdrawal wakes immediately. Otherwise the template remains usable throughout this
        // bounded observation window; the proof stays blocked until after the assertion.
        let _ = tokio::time::timeout(Duration::from_secs(1), publications.changed()).await;
        let current = cache
            .template_for_tip(candidate_hash, &network, DateTime32::now())
            .expect("an unseen publication notification must not withdraw live candidate work");
        assert_eq!(current.coinbase_txn, early.coinbase_txn);
        assert_eq!(tip.best_tip_hash(), Some(genesis_hash));
        assert!(candidates.is_current(candidate_hash));
        release_proof.send(()).unwrap();
        lock_task.await.unwrap();
        updater.abort_all();
        while updater.join_next().await.is_some() {}
    })
    .await
    .expect("publication/read interleaving must not wait for the speculative proof");
}
