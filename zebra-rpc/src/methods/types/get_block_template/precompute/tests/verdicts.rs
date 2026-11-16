//! Failed generations are withdrawn without discarding newer work or an independent valid fallback.

use super::*;

#[test]
fn rejected_work_without_a_valid_fallback_is_withdrawn() {
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let failed = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&failed));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_none());
    assert!(next_work(&cache).is_none());
    assert!(cache
        .template_for_tip(
            failed.previous_block_hash,
            &Network::Mainnet,
            failed.cur_time
        )
        .is_none());
}

#[test]
fn older_rejection_preserves_the_newer_served_generation_and_queued_check() {
    let cache = TemplateCache::default();
    cache.publish(template());
    cache.publish(full_template(0));
    let older = served_work(&cache).unwrap();
    cache.publish(full_template(1));
    let newer = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&older));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &newer)));
    assert!(next_work(&cache).is_some_and(|next| Arc::ptr_eq(&next, &newer)));
    assert!(!cache.reject(&older));
    assert!(!changes.take_changed());
}

#[test]
fn late_success_does_not_replace_a_newer_valid_fallback() {
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let older = served_work(&cache).unwrap();
    cache.publish(full_template(1));
    let valid = served_work(&cache).unwrap();
    assert!(cache.accept_validated(&valid, Some(&valid)));
    cache.publish(full_template(2));
    let failed = served_work(&cache).unwrap();

    assert!(!cache.accept_validated(&older, Some(&older)));
    assert!(cache.reject(&failed));
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &valid)));
}

#[test]
fn rejected_clock_narrowing_withdraws_its_source_generation() {
    for source_is_valid in [false, true] {
        let network = Network::Mainnet;
        let cache = TemplateCache::default();
        cache.publish(template());
        let coinbase_only = served_work(&cache).unwrap();
        cache.publish(full_template(0));
        let source = served_work(&cache).unwrap();
        if source_is_valid {
            assert!(cache.accept_validated(&source, Some(&source)));
        }
        let now = source
            .cur_time
            .saturating_sub(Duration32::from_hours(2))
            .saturating_sub(Duration32::from_seconds(1));
        let narrowed = cache
            .template_for_tip(source.previous_block_hash, &network, now)
            .unwrap();
        assert_ne!(narrowed.cur_time, source.cur_time);
        assert!(!Arc::ptr_eq(&source, &narrowed));
        let mut changes = cache.subscribe();

        assert!(cache.reject(&narrowed));
        assert!(changes.take_changed());
        assert!(next_work(&cache).is_none());
        if source_is_valid {
            assert!(cache
                .template_for_tip(source.previous_block_hash, &network, now)
                .is_none());
        } else {
            assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &coinbase_only)));
            assert!(cache
                .template_for_tip(source.previous_block_hash, &network, now)
                .is_some_and(|served| served.transactions.is_empty()));
        }
    }
}

#[test]
fn rejected_narrowed_fallback_is_not_restored_after_a_later_rejection() {
    let network = Network::Mainnet;
    let cache = TemplateCache::default();
    cache.publish(full_template(0));
    let source = served_work(&cache).unwrap();
    assert!(cache.accept_validated(&source, Some(&source)));
    let now = source
        .cur_time
        .saturating_sub(Duration32::from_hours(2))
        .saturating_sub(Duration32::from_seconds(1));
    let narrowed = cache
        .template_for_tip(source.previous_block_hash, &network, now)
        .unwrap();
    cache.publish(full_template(1));
    let newer = served_work(&cache).unwrap();
    let mut changes = cache.subscribe();

    assert!(cache.reject(&narrowed));
    assert!(changes.take_changed());
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &newer)));
    assert!(next_work(&cache).is_some_and(|next| Arc::ptr_eq(&next, &newer)));
    assert!(cache.reject(&newer));
    assert!(changes.take_changed());
    assert!(cache
        .template_for_tip(source.previous_block_hash, &network, now)
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn updater_recovers_withdrawn_work_without_bypassing_valid_fallback_pacing() {
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        for (has_fallback, during_check) in [(false, false), (false, true), (true, false), (true, true)] {
            let network = Network::Mainnet;
            let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let hash = block::Hash([1; 32]);
            let now = DateTime32::now();
            let chain_info = GetBlockTemplateChainInfo {
                expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
                tip_height: height,
                tip_hash: hash,
                cur_time: now,
                min_time: now,
                max_time: now.saturating_add(Duration32::from_minutes(90)),
                chain_history_root: fake_history_tree(&network).hash(),
                chain_value_pools: Default::default(),
            };
            let miner_params = MinerParams::from(
                Address::decode(
                    &network,
                    default_miner_address(network.kind(), &MinerAddressType::Transparent),
                )
                .unwrap(),
            );
            let transaction = |lock_height| {
                VerifiedUnminedTx::new(
                    Arc::new(Transaction::test_v1(
                        vec![],
                        vec![],
                        zebra_chain::transaction::LockTime::Height(Height(lock_height)),
                    ))
                    .into(),
                    10_000.try_into().unwrap(),
                    0,
                    0,
                    Arc::new(vec![]),
                )
                .unwrap()
            };
            let initial = transaction(1);
            let admitted = transaction(2);
            let coinbase_cache = CoinbaseCache::default();
            let cache = TemplateCache::default();
            let fallback = has_fallback.then(|| {
                cache.publish(BlockTemplateResponse::new_internal(
                    &network,
                    &coinbase_cache,
                    &miner_params,
                    &chain_info,
                    LongPollInput::new(height, hash, chain_info.max_time, std::iter::empty())
                        .generate_id(),
                    vec![],
                    None,
                ));
                served_work(&cache).unwrap()
            });
            let initial_generation = cache.publish(
                BlockTemplateResponse::from_mempool(
                    &network,
                    &coinbase_cache,
                    &miner_params,
                    &chain_info,
                    LongPollInput::new(height, hash, chain_info.max_time, [initial.transaction.id])
                        .generate_id(),
                    vec![initial.clone()],
                    Default::default(),
                )
                .unwrap(),
            );
            let read_state = tower::service_fn(move |request| {
                std::future::ready(Ok::<_, zebra_node_services::BoxError>(match request {
                    ReadRequest::Tip => ReadResponse::Tip(Some((height, hash))),
                    ReadRequest::ChainInfo => ReadResponse::ChainInfo(chain_info.clone()),
                    ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(None),
                    ReadRequest::MiningCandidateChanges => {
                        ReadResponse::MiningCandidateChanges(Default::default())
                    }
                    other => panic!("unexpected state request: {other:?}"),
                }))
            });
            let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> =
                MockService::build()
                    .with_max_request_delay(Duration::from_secs(10))
                    .for_unit_tests();
            let (tip, tip_sender) = MockChainTip::new();
            tip_sender.send_best_tip_height(height);
            tip_sender.send_best_tip_hash(hash);
            tip_sender.send_best_tip_block_time(chrono::Utc::now());
            let mut sync = MockSyncStatus::default();
            sync.set_is_close_to_tip(true);
            let (events, event_rx) = broadcast::channel(1);
            let mut publications = cache.subscribe();
            let updater = AbortOnDropHandle::new(tokio::spawn(run(
                network.clone(),
                miner_params,
                coinbase_cache,
                cache.clone(),
                mempool.clone(),
                event_rx,
                read_state,
                tip,
                sync,
                tower::service_fn(|_: VerifyRequest| {
                    std::future::pending::<Result<block::Hash, zebra_consensus::BoxError>>()
                }),
            )));
            let snapshot = mempool
                .expect_request(mempool::Request::FullTransactions)
                .await;
            let build_cost = MEMPOOL_THROTTLE * 2;
            tokio::time::advance(build_cost).await;
            snapshot.respond(mempool::Response::FullTransactions {
                transactions: vec![initial.clone()],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: hash,
            });
            let failed = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    if let Some(work) = served_work(&cache) {
                        if work.generation != initial_generation {
                            break work;
                        }
                    }
                    publications.changed().await;
                }
            })
            .await
            .expect("the updater must publish its initial full snapshot");
            let build_finished = cache.served.borrow().as_ref().unwrap().published_at;
            let mut held_membership = if during_check {
                events
                    .send(MempoolChange::invalidated(
                        [UnminedTxId::from_legacy_id(zebra_chain::transaction::Hash([9; 32]))]
                            .into_iter()
                            .collect(),
                    ))
                    .unwrap();
                tokio::time::advance(build_cost).await;
                Some(mempool.expect_request(mempool::Request::TransactionIds).await)
            } else {
                None
            };
            assert!(cache.reject(&failed));
            if let Some(fallback) = fallback {
                assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)));
                events
                    .send(MempoolChange::added(
                        [admitted.transaction.id].into_iter().collect(),
                    ))
                    .unwrap();
                let deadline = build_finished
                    + if during_check {
                        Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)
                    } else {
                        build_cost
                    };
                assert!(
                    tokio::time::timeout_at(
                        deadline - Duration::from_millis(1),
                        mempool.expect_request(mempool::Request::FullTransactions),
                    )
                    .await
                    .is_err(),
                    "restoring valid work must not cancel reconciliation or bypass build-cost pacing",
                );
                assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)));
                let membership = match held_membership.take() {
                    Some(membership) => membership,
                    None => mempool.expect_request(mempool::Request::TransactionIds).await,
                };
                membership.respond(mempool::Response::TransactionIds(
                    [initial.transaction.id, admitted.transaction.id]
                        .into_iter()
                        .collect(),
                ));
            } else {
                let recovered = tokio::time::timeout(Duration::from_millis(500), async {
                    loop {
                        if let Some(work) = cache.template_for_tip(hash, &network, now) {
                            break work;
                        }
                        publications.changed().await;
                    }
                })
                .await
                .expect("withdrawal must wake the real updater before its background deadline");
                assert!(recovered.transactions.is_empty());
                assert_ne!(recovered.generation, failed.generation);
            }
            let _replacement_snapshot = tokio::time::timeout(
                Duration::from_millis(500),
                mempool.expect_request(mempool::Request::FullTransactions),
            )
            .await
            .expect("reconstruction must proceed even while the older membership response is held");
            drop(updater);
        }
    })
    .await
    .expect("withdrawal recovery and fallback pacing must complete without stalled snapshots");
}

#[tokio::test]
async fn fallback_rejection_reaches_the_cache_across_updater_handoff() {
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        for has_valid_fallback in [false, true] {
            let cache = TemplateCache::default();
            let fallback = has_valid_fallback.then(|| {
                cache.publish(template());
                served_work(&cache).unwrap()
            });
            let mut verifier: MockService<
                VerifyRequest,
                block::Hash,
                _,
                zebra_consensus::BoxError,
            > = MockService::build()
                .with_max_request_delay(Duration::from_secs(10))
                .for_unit_tests();
            cache.publish_fallback(full_template(0), Network::Mainnet, verifier.clone());
            let failed = served_work(&cache).unwrap();
            let request = verifier
                .expect_request_that(|request| matches!(request, VerifyRequest::CheckProposal(_)))
                .await;
            assert_eq!(
                *request.request().block(),
                proposal_block_from_template(
                    &failed,
                    BlockTemplateTimeSource::CurTime,
                    &Network::Mainnet,
                )
                .unwrap()
            );
            let _updater = cache.claim_updater().unwrap();
            let mut changes = cache.subscribe();
            request.respond_error("rejected".into());
            changes.changed().await;
            assert_eq!(
                served_work(&cache).map(|served| served.generation),
                fallback.map(|valid| valid.generation),
            );
            assert!(next_work(&cache).is_none());
        }
    })
    .await
    .expect("fallback validation must withdraw rejected work after updater handoff");
}

/// Rejected unchanged membership cannot consume verifier work or release long polls in a hot loop.
#[tokio::test(start_paused = true)]
async fn rejected_unchanged_work_preserves_fallback_pacing() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
    let hash = block::Hash([1; 32]);
    let now = DateTime32::now();
    let chain_info = GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        tip_height: height,
        tip_hash: hash,
        cur_time: now,
        min_time: now,
        max_time: now.saturating_add(Duration32::from_minutes(90)),
        chain_history_root: fake_history_tree(&network).hash(),
        chain_value_pools: Default::default(),
    };
    let miner_params = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let coinbase_cache = CoinbaseCache::default();
    let fallback = BlockTemplateResponse::new_internal(
        &network,
        &coinbase_cache,
        &miner_params,
        &chain_info,
        LongPollInput::new(height, hash, chain_info.max_time, std::iter::empty()).generate_id(),
        vec![],
        None,
    );
    let read_state = tower::service_fn(move |request| {
        std::future::ready(Ok::<_, zebra_node_services::BoxError>(match request {
            ReadRequest::Tip => ReadResponse::Tip(Some((height, hash))),
            ReadRequest::ChainInfo => ReadResponse::ChainInfo(chain_info.clone()),
            ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(None),
            ReadRequest::MiningCandidateChanges => {
                ReadResponse::MiningCandidateChanges(Default::default())
            }
            other => panic!("unexpected state request: {other:?}"),
        }))
    });
    let transaction = |lock_height| {
        VerifiedUnminedTx::new(
            Arc::new(Transaction::test_v1(
                vec![],
                vec![],
                zebra_chain::transaction::LockTime::Height(Height(lock_height)),
            ))
            .into(),
            10_000.try_into().unwrap(),
            0,
            0,
            Arc::new(vec![]),
        )
        .unwrap()
    };
    let (pool, pool_rx) = watch::channel(vec![transaction(1)]);
    let (snapshot_delay, delay_rx) = watch::channel(Duration::ZERO);
    let mempool = tower::service_fn(move |request| {
        let transactions = pool_rx.borrow().clone();
        let delay = *delay_rx.borrow();
        async move {
            Ok::<_, zebra_node_services::BoxError>(match request {
                mempool::Request::FullTransactions => {
                    tokio::time::sleep(delay).await;
                    mempool::Response::FullTransactions {
                        transactions,
                        transaction_dependencies: Default::default(),
                        last_seen_tip_hash: hash,
                    }
                }
                mempool::Request::TransactionIds => mempool::Response::TransactionIds(
                    transactions.iter().map(|tx| tx.transaction.id).collect(),
                ),
                other => panic!("unexpected mempool request: {other:?}"),
            })
        }
    });
    // Bound observed work and hold the second check while an arrival is queued.
    let (checks, mut requests) = tokio::sync::mpsc::channel(1);
    let dispatched = Arc::new(Notify::new());
    let checked = dispatched.clone();
    let check_permits = Arc::new(tokio::sync::Semaphore::new(1));
    let permits = check_permits.clone();
    let verifier = tower::service_fn(move |request: VerifyRequest| {
        let checks = checks.clone();
        let checked = checked.clone();
        let permits = permits.clone();
        async move {
            checks.send((request.block(), Instant::now())).await?;
            checked.notify_one();
            permits.acquire_owned().await?.forget();
            Err::<block::Hash, zebra_consensus::BoxError>("rejected fee work".into())
        }
    });
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(height);
    tip_sender.send_best_tip_hash(hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now());
    let mut sync = MockSyncStatus::default();
    sync.set_is_close_to_tip(true);
    let cache = TemplateCache::default();
    cache.publish(fallback);
    let fallback = served_work(&cache).unwrap();
    let mut publications = cache.subscribe();
    let (events, event_rx) = broadcast::channel(1);
    let updater = AbortOnDropHandle::new(tokio::spawn(run(
        network.clone(),
        miner_params,
        coinbase_cache,
        cache.clone(),
        mempool,
        event_rx,
        read_state,
        tip,
        sync,
        verifier,
    )));
    tokio::time::timeout(Duration::from_secs(10), dispatched.notified())
        .await
        .expect("the initial fee-bearing build must be checked");
    tokio::time::timeout(Duration::from_secs(1), async {
        while !served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)) {
            publications.changed().await;
        }
    })
    .await
    .expect("a rejected build must restore the seeded valid fallback");
    publications.take_changed();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL) - Duration::from_millis(100),
            publications.changed(),
        )
        .await
        .is_err(),
        "unchanged rejected work must leave fallback long polls waiting for the baseline refresh",
    );
    assert!(served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)));
    // The next build's measured snapshot cost selects the existing one-second background pause.
    snapshot_delay.send_replace(MEMPOOL_THROTTLE);
    let (first, first_checked_at) = requests.recv().await.unwrap();
    assert_eq!(first.transactions.len(), 2);
    tokio::time::timeout(
        MEMPOOL_THROTTLE + Duration::from_secs(1),
        dispatched.notified(),
    )
    .await
    .expect("the existing periodic backstop must retry unchanged rejected work");
    let (_, next_checked_at) = requests.recv().await.unwrap();
    assert!(
        next_checked_at - first_checked_at >= Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL),
        "proposal checks for unchanged rejected membership must retain the baseline pacing",
    );

    let build_finished = cache.served.borrow().as_ref().unwrap().published_at;
    let next_background_build = build_finished + MEMPOOL_THROTTLE;
    snapshot_delay.send_replace(Duration::ZERO);
    let added = transaction(2);
    let added_id = added.transaction.id;
    pool.send_replace(vec![transaction(1), added]);
    events
        .send(MempoolChange::added([added_id].into_iter().collect()))
        .unwrap();
    publications.take_changed();
    check_permits.add_permits(1);
    tokio::time::timeout(Duration::from_millis(100), async {
        while !served_work(&cache).is_some_and(|served| Arc::ptr_eq(&served, &fallback)) {
            publications.changed().await;
        }
    })
    .await
    .expect("the gated rejection must restore valid fallback work");
    publications.take_changed();
    assert!(
        tokio::time::timeout_at(
            next_background_build - Duration::from_millis(1),
            publications.changed(),
        )
        .await
        .is_err(),
        "rejection cannot turn a queued arrival into demand that bypasses background pacing",
    );
    tokio::time::timeout(Duration::from_secs(1), dispatched.notified())
        .await
        .expect("the queued arrival must rebuild after the existing background pause");
    let (next, checked_at) = requests.recv().await.unwrap();
    assert_eq!(next.transactions.len(), 3);
    assert!(
        checked_at >= next_background_build,
        "a rejection wake cannot bypass the measured-cost background deadline",
    );
    drop(updater);
}
