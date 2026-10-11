//! Fixed test vectors for the precomputed block template cache.

use std::time::Duration;

use zcash_keys::address::Address;

use zebra_chain::{
    block,
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::MockChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    transaction::{Transaction, VerifiedUnminedTx},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, ParameterDifficulty, U256},
};
use zebra_state::GetBlockTemplateChainInfo;

use crate::{
    config::mining::{default_miner_address, MinerAddressType},
    methods::tests::utils::fake_history_tree,
};

use super::*;

/// Returns a template with an explicit timestamp boundary.
fn template_with_max_time(net: &Network, max_time: DateTime32) -> BlockTemplateResponse {
    let tip_height = NetworkUpgrade::Nu5
        .activation_height(net)
        .expect("Nu5 is active on the test network");

    let miner_params = MinerParams::from(
        Address::decode(
            net,
            default_miner_address(net.kind(), &MinerAddressType::Transparent),
        )
        .expect("hard-coded transparent address is valid"),
    );

    let chain_info = GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        tip_height,
        tip_hash: block::Hash([0xab; 32]),
        cur_time: DateTime32::from(1654008617),
        min_time: DateTime32::from(1654008606),
        max_time,
        chain_history_root: fake_history_tree(net).hash(),
        chain_value_pools: Default::default(),
    };

    let long_poll_id = LongPollInput::new(
        chain_info.tip_height,
        chain_info.tip_hash,
        chain_info.max_time,
        std::iter::empty(),
    )
    .generate_id();

    BlockTemplateResponse::new_internal(
        net,
        &CoinbaseCache::default(),
        &miner_params,
        &chain_info,
        long_poll_id,
        vec![],
        None,
    )
}

/// Returns a template for the notification and coinbase tests.
fn template() -> BlockTemplateResponse {
    template_with_max_time(&Network::Mainnet, DateTime32::from(1654008719))
}

/// Testnet difficulty changes strictly after max_time; other networks and minimum difficulty
/// remain usable even when wall time is beyond the template's timestamp range.
#[test]
fn only_current_work_is_served() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let max_time = DateTime32::from(1654008719);
    let after_max_time = max_time.saturating_add(Duration32::from_seconds(1));
    let cache = TemplateCache::default();
    let current = template_with_max_time(&network, max_time);
    let tip_hash = current.previous_block_hash;

    assert!(cache
        .template_for_tip(tip_hash, &network, max_time)
        .is_none());
    cache.publish(current);
    assert!(
        cache
            .template_for_tip(tip_hash, &network, max_time)
            .is_some(),
        "max_time is inclusive",
    );
    assert!(
        cache
            .template_for_tip(block::Hash([0xff; 32]), &network, max_time)
            .is_none(),
        "a template for another tip must never be served",
    );
    assert!(
        cache
            .template_for_tip(tip_hash, &network, after_max_time)
            .is_none(),
        "standard Testnet difficulty must be refreshed after the boundary",
    );

    // If the 90-minute median-time cap is reached before Testnet's difficulty boundary,
    // even a newly built template still uses standard difficulty and this same time range.
    let mut median_capped = template_with_max_time(&network, max_time);
    median_capped.min_time = max_time
        .saturating_sub(Duration32::from_minutes(90))
        .saturating_add(Duration32::from_seconds(1));
    cache.publish(median_capped);
    assert!(
        cache
            .template_for_tip(tip_hash, &network, after_max_time)
            .is_some(),
        "rebuilding cannot change difficulty beyond the median-time cap",
    );

    let mut minimum_difficulty = template_with_max_time(&network, max_time);
    minimum_difficulty.bits = network.target_difficulty_limit().to_compact();
    minimum_difficulty.target = network.target_difficulty_limit();
    cache.publish(minimum_difficulty);
    assert!(
        cache
            .template_for_tip(tip_hash, &network, after_max_time)
            .is_some(),
        "difficulty cannot become easier than the minimum",
    );

    let regtest = Network::new_regtest(
        zebra_chain::parameters::testnet::ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );
    for network in [Network::Mainnet, regtest] {
        cache.publish(template_with_max_time(&network, max_time));
        assert!(
            cache
                .template_for_tip(tip_hash, &network, after_max_time)
                .is_some(),
            "{network:?} does not change difficulty with wall time",
        );
    }
}

/// Before the MTP maximum activates, even a wide range can end at a difficulty boundary.
#[test]
fn wide_testnet_ranges_expire_before_mtp_maximum_activation() {
    let _init_guard = zebra_test::init();
    let configured = zebra_chain::parameters::testnet::Parameters::build()
        .with_slow_start_interval(block::Height(0))
        .with_activation_heights(
            zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu7: Some(299_188),
                ..Default::default()
            },
        )
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");
    let previous_time = DateTime32::from(1654008600);

    for (network, initial_gap) in [(Network::new_default_testnet(), 900), (configured, 450)] {
        for (height, gap, usable) in [
            (299_188, initial_gap, false),
            (653_605, 450, false),
            (653_606, 450, true),
        ] {
            let max_time = previous_time.saturating_add(Duration32::from_seconds(gap));
            let expired = max_time.saturating_add(Duration32::from_seconds(1));
            let cache = TemplateCache::default();
            let mut current = template_with_max_time(&network, max_time);
            current.height = height;
            current.min_time = max_time
                .saturating_sub(Duration32::from_minutes(90))
                .saturating_add(Duration32::from_seconds(1));
            let tip_hash = current.previous_block_hash;
            cache.publish(current);
            assert!(
                cache
                    .template_for_tip(tip_hash, &network, max_time)
                    .is_some(),
                "{network:?}, candidate height {height}: the difficulty boundary is inclusive",
            );
            assert_eq!(
                cache
                    .template_for_tip(tip_hash, &network, expired)
                    .is_some(),
                usable,
                "{network:?}, candidate height {height}",
            );
        }
    }
}

/// A clock rollback narrows usable ranges without changing their difficulty or work identity.
#[test]
fn clock_rollback_clamps_cached_timestamp_range() {
    let _init_guard = zebra_test::init();
    let regtest = Network::new_regtest(
        zebra_chain::parameters::testnet::ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );

    for network in [Network::Mainnet, Network::new_default_testnet(), regtest] {
        let cache = TemplateCache::default();
        let mut current = template_with_max_time(&network, DateTime32::from(1654008719));
        // The lower bound keeps Testnet minimum-difficulty work on its original side.
        current.bits = network.target_difficulty_limit().to_compact();
        current.target = network.target_difficulty_limit();
        let tip_hash = current.previous_block_hash;
        let boundary = current.max_time.saturating_sub(Duration32::from_hours(2));
        cache.publish(current.clone());

        for max_time in [
            current.max_time,
            current.max_time.saturating_sub(Duration32::from_seconds(1)),
            current.cur_time.saturating_sub(Duration32::from_seconds(1)),
            current.min_time,
            current.cur_time,
            current.max_time,
        ] {
            let now = max_time.saturating_sub(Duration32::from_hours(2));
            let clamped = cache
                .template_for_tip(tip_hash, &network, now)
                .expect("a usable range survives clock rollback");
            assert_eq!(clamped.max_time, max_time);
            assert_eq!(clamped.cur_time, current.cur_time.min(max_time));
            assert_eq!(clamped.min_time, current.min_time);
            assert_eq!(clamped.bits, current.bits);
            assert_eq!(clamped.target, current.target);
            assert_eq!(clamped.long_poll_id, current.long_poll_id);
        }
        let impossible = current
            .min_time
            .saturating_sub(Duration32::from_hours(2))
            .saturating_sub(Duration32::from_seconds(1));
        assert!(cache
            .template_for_tip(tip_hash, &network, impossible)
            .is_none());
        assert_eq!(
            cache
                .template_for_tip(tip_hash, &network, boundary)
                .expect("a narrowed response does not change the cached snapshot")
                .max_time,
            current.max_time,
        );
    }
}

/// Checks that a subscription taken before a template is published still reports it.
///
/// `getblocktemplate` reads the cache, decides the client already has that template, and only then
/// waits. A subscription taken at the point of waiting would mark anything published in between as
/// seen, and the caller's other wake conditions are a chain tip change and `max_time`, neither of
/// which fires when the mempool alone changes: it would sit on a template it had already been told
/// about until the updater's backstop.
#[tokio::test]
async fn a_subscription_reports_a_template_published_before_the_wait() {
    let _init_guard = zebra_test::init();

    let cache = TemplateCache::default();

    // The order the RPC uses: subscribe, read, then wait.
    let mut changes = cache.subscribe();
    assert!(cache.is_empty(), "nothing is published yet");

    cache.publish(template());

    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .expect("a template published before the wait should still end it");
}

/// Checks that a subscription reports each later publish, so a long poll that loops keeps waiting
/// on templates it hasn't seen rather than on the one it just read.
#[tokio::test]
async fn a_subscription_reports_each_later_publish() {
    let _init_guard = zebra_test::init();

    let cache = TemplateCache::default();
    let mut changes = cache.subscribe();

    for _ in 0..3 {
        cache.publish(template());

        tokio::time::timeout(Duration::from_secs(10), changes.changed())
            .await
            .expect("each publish should end a wait");
    }

    // With nothing published since, the next wait doesn't return.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), changes.changed())
            .await
            .is_err(),
        "a subscription that has seen every publish should keep waiting",
    );
}

/// A reorg must not detach a proof that can still finish and be reused at its original height.
#[tokio::test]
async fn in_flight_coinbase_is_retained_across_height_changes() {
    let _init_guard = zebra_test::init();
    let net = Network::Mainnet;
    let miner_params = MinerParams::from(
        Address::decode(
            &net,
            default_miner_address(net.kind(), &MinerAddressType::Transparent),
        )
        .expect("hard-coded transparent address is valid"),
    );
    let template = template();
    let height = Height(template.height);
    let other_height = height.next().expect("test height is below the maximum");
    let coinbase = template.coinbase_txn;
    let expected_coinbase = coinbase.clone();
    let cache = CoinbaseCache::default();
    let (release_proof, proof_released) = tokio::sync::oneshot::channel();
    let mut next_coinbase = Some((
        height,
        tokio::task::spawn_blocking(move || {
            proof_released
                .blocking_recv()
                .expect("the test releases the proof before awaiting it");
            coinbase
        }),
    ));

    tokio::time::timeout(Duration::from_secs(10), async {
        // Neither consuming at a different height nor starting the next proof may detach this one.
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        start_precomputing_coinbase(&mut next_coinbase, &net, &miner_params, other_height);
        release_proof
            .send(())
            .expect("the in-flight proof is still waiting");
        store_precomputed_coinbase(&mut next_coinbase, height, &cache).await;

        assert_eq!(
            cache.get(height, Amount::zero(), None),
            Some(expected_coinbase),
            "returning to the original height must reuse the tracked proof"
        );
        assert!(
            cache.get(other_height, Amount::zero(), None).is_none(),
            "a proof must not be stored under a different height"
        );
    })
    .await
    .expect("the retained proof should complete once released");
}

/// Finished work for the wrong height must not enter the cache or prevent the next proof.
#[tokio::test]
async fn completed_coinbase_is_replaced_without_caching_the_wrong_height() {
    let _init_guard = zebra_test::init();
    let net = Network::Mainnet;
    let miner_params = MinerParams::from(
        Address::decode(
            &net,
            default_miner_address(net.kind(), &MinerAddressType::Transparent),
        )
        .expect("hard-coded transparent address is valid"),
    );
    let height = Height(template().height);
    let other_height = height.next().expect("test height is below the maximum");
    let cache = CoinbaseCache::default();
    let mut next_coinbase = None;
    start_precomputing_coinbase(&mut next_coinbase, &net, &miner_params, height);

    tokio::time::timeout(Duration::from_secs(10), async {
        while !next_coinbase
            .as_ref()
            .expect("a proof was started")
            .1
            .is_finished()
        {
            tokio::task::yield_now().await;
        }

        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert!(cache.get(height, Amount::zero(), None).is_none());
        assert!(cache.get(other_height, Amount::zero(), None).is_none());

        start_precomputing_coinbase(&mut next_coinbase, &net, &miner_params, other_height);
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert_eq!(
            cache.get(other_height, Amount::zero(), None),
            Some(
                TransactionTemplate::new_coinbase(
                    &net,
                    other_height,
                    &miner_params,
                    Amount::zero()
                )
                .expect("test parameters produce a valid coinbase")
            ),
            "completed stale work must not prevent a proof at the new height"
        );
    })
    .await
    .expect("the replacement proof should complete");
}

/// Checks that a shielded miner address waits longer for a precomputed template than a transparent
/// one, because falling back to an on-demand build runs a coinbase proof per request.
#[test]
fn shielded_miner_addresses_wait_longer_for_a_template() {
    let _init_guard = zebra_test::init();

    let net = Network::new_default_testnet();

    let miner_params = |addr_type| {
        MinerParams::from(
            Address::decode(&net, default_miner_address(net.kind(), &addr_type))
                .expect("hard-coded miner address is valid"),
        )
    };

    let transparent = miner_params(MinerAddressType::Transparent);
    assert!(!transparent.has_shielded_component());
    assert_eq!(new_tip_timeout(&transparent), NEW_TIP_TIMEOUT);

    for (name, addr_type) in [
        ("a Sapling address", MinerAddressType::Sapling),
        ("a unified address", MinerAddressType::Unified),
    ] {
        let shielded = miner_params(addr_type);
        assert!(
            shielded.has_shielded_component(),
            "{name} pays a shielded coinbase output"
        );
        assert_eq!(
            new_tip_timeout(&shielded),
            SHIELDED_NEW_TIP_TIMEOUT,
            "{name} should wait for the updater instead of proving per request"
        );
    }
}

/// Rejections and overflow do not rebuild unchanged work, but collateral evictions do.
#[tokio::test(start_paused = true)]
async fn mempool_changes_compare_the_verified_set() {
    let old_id = UnminedTxId::from_legacy_id(zebra_chain::transaction::Hash([1; 32]));
    let rejected_id = UnminedTxId::from_legacy_id(zebra_chain::transaction::Hash([2; 32]));
    let template_ids: HashSet<_> = [old_id].into_iter().collect();
    let (pool, snapshot) = watch::channel(template_ids.clone());
    let mempool = tower::service_fn(move |request| {
        assert_eq!(request, mempool::Request::TransactionIds);
        std::future::ready(Ok::<_, zebra_node_services::BoxError>(
            mempool::Response::TransactionIds(snapshot.borrow().clone()),
        ))
    });
    let (events, mut receiver) = broadcast::channel(1);
    let changed = wait_for_mempool_change(&mut receiver, mempool, &template_ids, Instant::now());
    tokio::pin!(changed);

    for _ in 0..20 {
        events
            .send(MempoolChange::invalidated(
                [rejected_id].into_iter().collect(),
            ))
            .unwrap();
    }
    assert!(
        futures::poll!(&mut changed).is_pending(),
        "overflowed rejection notifications did not change the verified set",
    );

    pool.send_replace(HashSet::new());
    events
        .send(MempoolChange::invalidated(
            [rejected_id].into_iter().collect(),
        ))
        .unwrap();
    tokio::time::advance(MEMPOOL_THROTTLE).await;
    tokio::time::timeout(Duration::from_millis(1), changed)
        .await
        .expect("a collateral eviction must wake the updater even when its ID was not announced");
}

/// Slow builds get an equal idle interval, including the backstop, but tip changes bypass it.
#[tokio::test(start_paused = true)]
async fn slow_builds_pace_mempool_refresh_without_delaying_tip_changes() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
    let hash = block::Hash([1; 32]);
    let now = DateTime32::now();
    let slow_build = Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL + 1);
    let transaction = VerifiedUnminedTx::new(
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

    for change_tip in [false, true] {
        let (state_tip, state_tip_rx) = watch::channel(hash);
        let read_state = tower::service_fn({
            let state_tip_rx = state_tip_rx.clone();
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
            move |request| {
                let mut chain_info = chain_info.clone();
                chain_info.tip_hash = *state_tip_rx.borrow();
                std::future::ready(Ok::<_, zebra_node_services::BoxError>(match request {
                    ReadRequest::Tip => ReadResponse::Tip(Some((height, chain_info.tip_hash))),
                    ReadRequest::ChainInfo => ReadResponse::ChainInfo(chain_info),
                    ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(None),
                    ReadRequest::MiningCandidateChanges => {
                        ReadResponse::MiningCandidateChanges(Default::default())
                    }
                    other => panic!("unexpected state request: {other:?}"),
                }))
            }
        });
        let (pool, pool_rx) = watch::channel(Vec::<VerifiedUnminedTx>::new());
        let (build_started, mut builds) = watch::channel(0);
        let mempool = tower::service_fn(move |request| {
            let (response, full_read) = match request {
                mempool::Request::TransactionIds => (
                    mempool::Response::TransactionIds(
                        pool_rx
                            .borrow()
                            .iter()
                            .map(|tx| tx.transaction.id)
                            .collect(),
                    ),
                    false,
                ),
                mempool::Request::FullTransactions => {
                    build_started.send_modify(|count| *count += 1);
                    (
                        mempool::Response::FullTransactions {
                            transactions: pool_rx.borrow().clone(),
                            transaction_dependencies: Default::default(),
                            last_seen_tip_hash: *state_tip_rx.borrow(),
                        },
                        true,
                    )
                }
                other => panic!("unexpected mempool request: {other:?}"),
            };
            async move {
                if full_read {
                    sleep(slow_build).await;
                }
                Ok::<_, zebra_node_services::BoxError>(response)
            }
        });
        let (tip, tip_sender) = MockChainTip::new();
        tip_sender.send_best_tip_height(height);
        tip_sender.send_best_tip_hash(hash);
        tip_sender.send_best_tip_block_time(chrono::Utc::now());
        let mut sync = MockSyncStatus::default();
        sync.set_is_close_to_tip(true);
        let miner_params = MinerParams::from(
            Address::decode(
                &network,
                default_miner_address(network.kind(), &MinerAddressType::Transparent),
            )
            .unwrap(),
        );
        let cache = TemplateCache::default();
        let mut publications = cache.subscribe();
        let (events, receiver) = broadcast::channel(1);
        let updater = tokio::spawn(run(
            network.clone(),
            miner_params,
            CoinbaseCache::default(),
            cache,
            mempool,
            receiver,
            read_state,
            tip,
            sync,
            // Background validation is idle work here; it must not affect pacing.
            tower::service_fn(|_: VerifyRequest| {
                std::future::pending::<Result<block::Hash, zebra_consensus::BoxError>>()
            }),
        ));
        tokio::time::timeout(Duration::from_secs(1), builds.changed())
            .await
            .expect("the initial full build starts")
            .unwrap();
        publications.changed().await;
        tokio::time::advance(slow_build).await;
        tokio::time::timeout(Duration::from_millis(1), publications.changed())
            .await
            .expect("the initial full template is published");
        let finished = Instant::now();

        pool.send_replace(vec![transaction.clone()]);
        events
            .send(MempoolChange::added(
                [transaction.transaction.id].into_iter().collect(),
            ))
            .unwrap();
        if change_tip {
            let new_hash = block::Hash([2; 32]);
            state_tip.send_replace(new_hash);
            tip_sender.send_best_tip_hash(new_hash);
        } else {
            tokio::time::advance(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)).await;
            assert_eq!(*builds.borrow(), 1, "the backstop must not bypass pacing");
            tokio::time::advance(
                slow_build
                    - Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)
                    - Duration::from_millis(1),
            )
            .await;
            assert_eq!(
                *builds.borrow(),
                1,
                "the mempool must wait for the full pause"
            );
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        tokio::time::timeout(Duration::from_millis(1), builds.changed())
            .await
            .expect("the next build starts at its deadline, or immediately for a new tip")
            .unwrap();
        assert_eq!(
            finished.elapsed(),
            if change_tip {
                Duration::ZERO
            } else {
                slow_build
            },
        );
        updater.abort();
    }
}

/// State visibility can precede the tip watch, leaving a mismatched snapshot in retry backoff.
#[tokio::test(start_paused = true)]
async fn tip_changes_interrupt_template_retry_backoff() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let initial = template();
    let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
    let parent = initial.previous_block_hash;
    let committed = block::Hash([2; 32]);
    let now = DateTime32::now();
    let chain_info = GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        tip_height: height.next().unwrap(),
        tip_hash: committed,
        cur_time: now,
        min_time: now,
        max_time: now.saturating_add(Duration32::from_minutes(90)),
        chain_history_root: fake_history_tree(&network).hash(),
        chain_value_pools: Default::default(),
    };
    let read_state = tower::service_fn(move |request| {
        std::future::ready(Ok::<_, zebra_node_services::BoxError>(match request {
            ReadRequest::Tip => ReadResponse::Tip(Some((chain_info.tip_height, committed))),
            ReadRequest::ChainInfo => ReadResponse::ChainInfo(chain_info.clone()),
            ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(None),
            ReadRequest::MiningCandidateChanges => {
                ReadResponse::MiningCandidateChanges(Default::default())
            }
            other => panic!("unexpected state request: {other:?}"),
        }))
    });
    let mempool = tower::service_fn(move |request| {
        assert!(matches!(request, mempool::Request::FullTransactions));
        std::future::ready(Ok::<_, zebra_node_services::BoxError>(
            mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: parent,
            },
        ))
    });
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(height);
    tip_sender.send_best_tip_hash(parent);
    tip_sender.send_best_tip_block_time(chrono::Utc::now());
    let mut sync = MockSyncStatus::default();
    sync.set_is_close_to_tip(true);
    let miner = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let cache = TemplateCache::default();
    cache.publish(initial);
    let mut changes = cache.subscribe();
    let (_events, events) = broadcast::channel(1);
    let updater = run(
        network,
        miner,
        CoinbaseCache::default(),
        cache.clone(),
        mempool,
        events,
        read_state,
        tip,
        sync,
        tower::service_fn(|_: VerifyRequest| {
            std::future::pending::<Result<block::Hash, zebra_consensus::BoxError>>()
        }),
    );
    tokio::pin!(updater);
    // State already exposes the new parent, but the tip watch and mempool still expose the old
    // parent. The first poll reaches the mismatched-snapshot retry without starting a proof.
    assert!(futures::poll!(&mut updater).is_pending());
    assert!(!cache.holds_tip(committed));
    tip_sender.send_best_tip_height(height.next().unwrap());
    tip_sender.send_best_tip_hash(committed);
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = &mut updater => panic!("the updater must keep running"),
            _ = async {
                while !cache.holds_tip(committed) {
                    changes.changed().await;
                }
            } => {}
        }
    })
    .await
    .expect("the new tip must receive coinbase-only work");
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "a pending tip change must interrupt retry backoff"
    );
}

#[tokio::test]
async fn background_proposals_populate_reuse_cache_and_coalesce_while_busy() {
    use futures::FutureExt;
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use zebra_chain::{
        amount::DeferredPoolBalanceChange,
        transaction::{LockTime, Transaction},
        transparent,
    };
    use zebra_state::{ContextuallyVerifiedBlock, Request, Response, SemanticallyVerifiedBlock};
    use zebra_test::mock_service::MockService;
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let network = zebra_chain::parameters::testnet::Parameters::build()
            .with_disable_pow(true)
            .to_network()
            .unwrap();
        let mut full = template_with_max_time(&network, DateTime32::from(1654008719));
        let empty = full.clone();
        let outpoint = transparent::OutPoint {
            hash: [7; 32].into(),
            index: 0,
        };
        let output =
            transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&[0x51]));
        let utxo = transparent::OrderedUtxo::new(output.clone(), Height(1), 1);
        let transaction = Transaction::test_v4(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            vec![output],
            LockTime::unlocked(),
            Height(0),
        );
        full.transactions.push(TransactionTemplate {
            data: (&transaction).into(),
            hash: transaction.hash(),
            auth_digest: block::merkle::AUTH_DIGEST_PLACEHOLDER,
            depends: vec![],
            fee: Amount::zero(),
            sigops: 0,
            required: false,
        });
        full.default_roots.merkle_root = [full.coinbase_txn.hash, transaction.hash()]
            .into_iter()
            .collect();
        let reused = Arc::new(AtomicUsize::new(0));
        let state = tower::service_fn({
            let network = network.clone();
            let reused = reused.clone();
            let parent = (Height(full.height - 1), full.previous_block_hash);
            move |request| {
                let network = network.clone();
                let utxo = utxo.clone();
                let reused = reused.clone();
                async move {
                    Ok::<_, zebra_consensus::BoxError>(match request {
                        Request::Tip => Response::Tip(Some(parent)),
                        Request::BestChainBlockHash(_) => Response::BlockHash(None),
                        Request::KnownBlock(_) => Response::KnownBlock(None),
                        Request::AwaitUtxo(requested) => {
                            assert_eq!(requested, outpoint);
                            Response::Utxo(utxo.utxo)
                        }
                        Request::CheckBlockProposalValidity(block) => {
                            Response::ValidBlockProposal(Box::new(
                                ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                                    block,
                                    HashMap::from([(outpoint, utxo)]),
                                    DeferredPoolBalanceChange::zero(),
                                    &network,
                                    Default::default(),
                                )
                                .unwrap(),
                            ))
                        }
                        Request::CommitContextuallyVerifiedBlock(block) => {
                            reused.fetch_add(1, Ordering::SeqCst);
                            let block: SemanticallyVerifiedBlock = (*block).into();
                            Response::Committed(block.hash)
                        }
                        other => panic!("unexpected state request: {other:?}"),
                    })
                }
            }
        });
        let (router, _, _handles, _) = zebra_consensus::router::init_test(
            zebra_consensus::Config {
                checkpoint_sync: false,
            },
            &network,
            state,
        )
        .await;
        let cache = TemplateCache::default();
        // Proposal conversion runs off-thread; the outer deadline bounds its mock requests.
        let mut verifier: MockService<VerifyRequest, block::Hash, _, zebra_consensus::BoxError> =
            MockService::build()
                .with_max_request_delay(Duration::MAX)
                .for_unit_tests();
        let mut verifier_requests = verifier.clone();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(validate_templates(
            cache.subscribe(),
            network.clone(),
            verifier.clone(),
        ));
        cache.publish(empty);
        // The empty template must not reach the verifier.
        // Paused sleeps advance only after runnable work and blocking conversions are idle.
        tokio::time::pause();
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::time::resume();
        assert!(
            tasks.try_join_next().is_none(),
            "coinbase-only templates must leave the validator running"
        );
        assert!(
            verifier_requests
                .try_next_request()
                .now_or_never()
                .is_none(),
            "coinbase-only templates must not reach the verifier"
        );
        let expected =
            proposal_block_from_template(&full, BlockTemplateTimeSource::CurTime, &network)
                .unwrap();
        cache.publish(full.clone());
        let first = verifier
            .expect_request(VerifyRequest::CheckProposal(Arc::new(expected.clone())))
            .await;
        full.cur_time = full.cur_time.saturating_add(Duration32::from_seconds(1));
        cache.publish(full.clone());
        full.cur_time = full.cur_time.saturating_add(Duration32::from_seconds(1));
        cache.publish(full.clone());
        tokio::time::pause();
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::time::resume();
        assert!(
            tasks.try_join_next().is_none(),
            "pending verification must leave the validator running"
        );
        assert!(
            verifier_requests
                .try_next_request()
                .now_or_never()
                .is_none(),
            "the next proposal must wait for the current verification"
        );
        // Forward the worker's request through the real router, populating its consensus cache.
        first.respond(
            router
                .clone()
                .oneshot(VerifyRequest::CheckProposal(Arc::new(expected.clone())))
                .await
                .unwrap(),
        );
        assert_eq!(
            router
                .clone()
                .oneshot(VerifyRequest::Commit(Arc::new(expected.clone())))
                .await
                .unwrap(),
            expected.hash()
        );
        assert_eq!(reused.load(Ordering::SeqCst), 1);
        let latest =
            proposal_block_from_template(&full, BlockTemplateTimeSource::CurTime, &network)
                .unwrap();
        verifier
            .expect_request(VerifyRequest::CheckProposal(Arc::new(latest.clone())))
            .await
            .respond(latest.hash());
        tasks.abort_all();
    })
    .await
    .expect("background validation must not stall publication");
}

/// Pending transaction verification permits early work; rejection restores the committed parent.
#[tokio::test]
async fn pending_verification_publishes_early_work_and_failure_restores_the_tip() {
    use zebra_chain::{block::genesis::regtest_genesis_block, chain_sync_status::MockSyncStatus};
    use zebra_consensus::{error::TransactionError, SemanticBlockVerifier, VerifyBlockError};
    use zebra_node_services::mempool;
    use zebra_test::mock_service::MockService;

    use crate::methods::types::get_block_template::proposal::proposal_block_from_template;

    let _init_guard = zebra_test::init();

    // Stay below the candidate's 30-second lifetime: expiry must not imitate failure withdrawal.
    tokio::time::timeout(Duration::from_secs(20), async {
        let network = Network::new_regtest(Default::default());
        let genesis = regtest_genesis_block();
        let tip_hash = genesis.hash();
        let (state, read_state, tip, _tip_changes) =
            zebra_state::populated_state([genesis], &network).await;
        let miner_params = MinerParams::from(
            Address::decode(
                &network,
                default_miner_address(network.kind(), &MinerAddressType::Transparent),
            )
            .expect("hard-coded transparent address is valid"),
        );
        let mut mempool: MockService<_, _, _, zebra_state::BoxError> = MockService::build()
            .with_max_request_delay(Duration::from_secs(10))
            .for_unit_tests();
        let mut transactions: MockService<_, _, _, zebra_state::BoxError> = MockService::build()
            .with_max_request_delay(Duration::from_secs(10))
            .for_unit_tests();
        let cache = TemplateCache::default();
        let mut changes = cache.subscribe();
        let updater = tokio::spawn(run(
            network.clone(),
            miner_params,
            CoinbaseCache::default(),
            cache.clone(),
            mempool.clone(),
            read_state.clone(),
            tip.clone(),
            MockSyncStatus::default(),
        ));

        let original = loop {
            if let Some(template) = cache.template_for_tip(tip_hash, &network, DateTime32::now()) {
                break template;
            }
            changes.changed().await;
        };
        assert_eq!(original.height, 1);

        // Keep the full build pending too, so the candidate must interrupt an active updater.
        let mempool_response = mempool
            .expect_request(mempool::Request::FullTransactions)
            .await;
        let candidate = Arc::new(
            proposal_block_from_template(&original, None, &network)
                .expect("the real Regtest template produces a candidate"),
        );
        assert_eq!(candidate.transactions.len(), 1);
        let candidate_hash = candidate.hash();
        let ReadResponse::MiningCandidateChanges(mut candidates) = read_state
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .expect("candidate changes are readable")
        else {
            panic!("state must return a candidate subscription");
        };
        let verification = tokio::spawn(
            SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
                .oneshot(zebra_consensus::Request::Commit(candidate.clone())),
        );
        let transaction_response = transactions
            .expect_request_that(|request| {
                request.transaction == candidate.transactions[0] && request.height == Height(1)
            })
            .await;

        let info = loop {
            if let ReadResponse::MiningCandidate(Some(info)) = read_state
                .clone()
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .expect("the candidate notification is readable")
            {
                break info;
            }
            candidates.changed().await;
        };
        assert_eq!(info.tip_hash, candidate_hash);
        assert_eq!(info.tip_height, Height(1));

        let early = loop {
            if let Some(template) =
                cache.template_for_tip(candidate_hash, &network, DateTime32::now())
            {
                break template;
            }
            changes.changed().await;
        };
        assert!(
            !verification.is_finished(),
            "no transaction response has been sent"
        );
        assert_eq!(early.previous_block_hash, candidate_hash);
        assert_eq!(early.height, 2);
        assert!(
            early.transactions.is_empty(),
            "early work must be coinbase-only"
        );
        assert_eq!(early.bits, info.expected_difficulty);
        assert_eq!(early.min_time, info.min_time);
        assert_eq!(early.max_time, info.max_time);
        let early_block = proposal_block_from_template(&early, None, &network)
            .expect("early work must contain a usable coinbase transaction");
        assert_eq!(early_block.transactions.len(), 1);
        assert_eq!(early_block.coinbase_height(), Some(Height(2)));
        assert_eq!(tip.best_tip_height_and_hash(), Some((Height(0), tip_hash)));

        mempool_response.respond(mempool::Response::FullTransactions {
            transactions: Vec::new(),
            transaction_dependencies: Default::default(),
            last_seen_tip_hash: tip_hash,
        });
        transaction_response.respond_error(TransactionError::CoinbasePosition.into());
        assert!(matches!(
            verification
                .await
                .expect("the verifier task must not panic"),
            Err(VerifyBlockError::Transaction(
                TransactionError::CoinbasePosition
            ))
        ));
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .expect("candidate withdrawal is readable"),
            ReadResponse::MiningCandidate(None)
        ));

        let restored = loop {
            if let Some(template) = cache.template_for_tip(tip_hash, &network, DateTime32::now()) {
                break template;
            }
            changes.changed().await;
        };
        assert_eq!(restored.height, original.height);
        assert_eq!(restored.previous_block_hash, tip_hash);
        assert!(cache
            .template_for_tip(candidate_hash, &network, DateTime32::now())
            .is_none());
        assert!(matches!(
            read_state
                .oneshot(ReadRequest::Tip)
                .await
                .expect("the committed tip is readable"),
            ReadResponse::Tip(Some((Height(0), hash))) if hash == tip_hash
        ));

        updater.abort();
        assert!(updater
            .await
            .expect_err("the updater runs until aborted")
            .is_cancelled());
    })
    .await
    .expect("candidate publication and rejection rollback must finish before candidate expiry");
}
