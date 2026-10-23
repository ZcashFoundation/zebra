//! Candidate transitions must not lose usable work or reserve obsolete speculation.

use zebra_test::mock_service::MockService;

use super::*;

#[tokio::test]
async fn committing_during_a_fee_proof_keeps_coinbase_only_work_available() {
    candidate_transition(false).await;
}

#[tokio::test]
async fn replacing_a_candidate_cancels_its_pending_snapshot_and_starts_the_next_proof() {
    candidate_transition(true).await;
}

async fn published_template(
    cache: &TemplateCache,
    changes: &mut TemplateChanges,
    hash: block::Hash,
    network: &Network,
) -> Arc<BlockTemplateResponse> {
    loop {
        if let Some(template) = cache.template_for_tip(hash, network, DateTime32::now()) {
            return template;
        }
        changes.changed().await;
    }
}

async fn candidate_transition(rejected: bool) {
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
        let miner = MinerParams::from(
            Address::decode(
                &network,
                default_miner_address(network.kind(), &MinerAddressType::Sapling),
            )
            .unwrap(),
        );
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
        let snapshot = |hash| mempool::Response::FullTransactions {
            transactions: vec![survivor.clone()],
            transaction_dependencies: Default::default(),
            last_seen_tip_hash: hash,
        };
        let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> = MockService::build()
            .with_max_request_delay(Duration::from_secs(3))
            .for_unit_tests();
        let cache = TemplateCache::default();
        let coinbases = CoinbaseCache::default();
        let mut publications = cache.subscribe();
        let (_events, events) = broadcast::channel(1);
        let verifier = tower::service_fn(|request: VerifyRequest| async move {
            Ok::<_, zebra_consensus::BoxError>(request.block().hash())
        });
        // Dropping the set on a failed assertion also stops the updater.
        let mut updater = tokio::task::JoinSet::new();
        updater.spawn(run(
            network.clone(),
            miner,
            coinbases.clone(),
            cache.clone(),
            mempool.clone(),
            events,
            read_state.clone(),
            tip.clone(),
            MockSyncStatus::default(),
            verifier,
        ));
        let initial = published_template(&cache, &mut publications, genesis_hash, &network).await;
        let _old_tip_snapshot = mempool
            .expect_request(mempool::Request::FullTransactions)
            .await;
        let candidate = Arc::new(
            proposal_block_from_template(&initial, BlockTemplateTimeSource::CurTime, &network)
                .unwrap(),
        );
        let candidate_hash = candidate.hash();
        let zebra_state::Response::MiningCandidate(Some(candidate_guard)) = state
            .clone()
            .oneshot(zebra_state::Request::MiningCandidate(candidate.clone()))
            .await
            .unwrap()
        else {
            panic!("the updater's subscription enables candidates");
        };
        let early = published_template(&cache, &mut publications, candidate_hash, &network).await;
        assert!(early.transactions.is_empty());
        let candidate_snapshot = mempool
            .expect_request(mempool::Request::FullTransactions)
            .await;

        if rejected {
            // Keep A's response pending, so only cancellation can release its speculation slot.
            drop(candidate_guard);
            let mut replacement = (*candidate).clone();
            Arc::make_mut(&mut replacement.header).nonce = [1; 32].into();
            let replacement = Arc::new(replacement);
            let replacement_hash = replacement.hash();
            let _replacement_guard = loop {
                if let zebra_state::Response::MiningCandidate(Some(guard)) = state
                    .clone()
                    .oneshot(zebra_state::Request::MiningCandidate(replacement.clone()))
                    .await
                    .unwrap()
                {
                    break guard;
                }
                tokio::task::yield_now().await;
            };
            published_template(&cache, &mut publications, replacement_hash, &network).await;
            // This deadline is shorter than A's snapshot timeout: expiry cannot imitate cancellation.
            let replacement_snapshot = tokio::time::timeout(
                Duration::from_secs(2),
                mempool.expect_request(mempool::Request::FullTransactions),
            )
            .await
            .expect("the replacement must start without awaiting A's pending snapshot");
            replacement_snapshot.respond(snapshot(genesis_hash));
            loop {
                if coinbases.get(Height(2), survivor.miner_fee, None).is_some() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
            assert!(cache
                .template_for_tip(replacement_hash, &network, DateTime32::now())
                .is_some());
            drop(candidate_snapshot);
        } else {
            // Stall the actual speculative blocking worker at its cache lookup, after it owns the
            // proving permit. State commitment and cached template reads do not need this mutex.
            let entries = coinbases.entries.clone();
            let (locked, lock_acquired) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            let lock_task = tokio::task::spawn_blocking(move || {
                let _guard = entries.lock().unwrap();
                locked.send(()).unwrap();
                assert_ne!(
                    released.recv_timeout(Duration::from_secs(60)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout),
                    "the committed snapshot must arrive before the proof watchdog expires",
                );
            });
            lock_acquired.await.unwrap();
            candidate_snapshot.respond(snapshot(genesis_hash));
            while coinbases.build.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
            // The live blocking task prevents auto-advance; its watchdog uses real time. Pause only
            // after the worker owns the permit, so the loop above keeps the enclosing timeout.
            tokio::time::pause();
            state
                .clone()
                .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
                    candidate.into(),
                ))
                .await
                .unwrap();
            drop(candidate_guard);
            // Reading the committed mempool proves the updater has processed the transition. The
            // old implementation instead clears early work and waits on the blocked proving permit.
            let full_snapshot = mempool
                .expect_request(mempool::Request::FullTransactions)
                .await;
            tokio::time::resume();
            let committed = cache
                .template_for_tip(candidate_hash, &network, DateTime32::now())
                .expect(
                    "the committed parent's coinbase-only work must remain immediately available",
                );
            assert!(committed.transactions.is_empty());
            assert_eq!(committed.coinbase_txn, early.coinbase_txn);
            full_snapshot.respond(snapshot(candidate_hash));
            release.send(()).unwrap();
            lock_task.await.unwrap();
        }
        updater.abort_all();
        while updater.join_next().await.is_some() {}
    })
    .await
    .expect("candidate transitions must complete without waiting for snapshot expiry");
}
