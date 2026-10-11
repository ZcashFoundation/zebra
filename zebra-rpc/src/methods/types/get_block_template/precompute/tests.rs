//! Fixed test vectors for the precomputed block template cache.

use std::time::Duration;

use zcash_keys::address::Address;

use zebra_chain::{
    block,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
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

/// A clock rollback must invalidate any template advertising timestamps beyond the new bound.
#[test]
fn clock_rollback_invalidates_cached_timestamp_range() {
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
        let current = template_with_max_time(&network, DateTime32::from(1654008719));
        let tip_hash = current.previous_block_hash;
        let boundary = current.max_time.saturating_sub(Duration32::from_hours(2));
        let rollback = boundary.saturating_sub(Duration32::from_seconds(1));
        // Checking only cur_time would miss the invalid advertised maximum.
        assert!(current.cur_time <= rollback.saturating_add(Duration32::from_hours(2)));
        cache.publish(current);

        assert!(cache
            .template_for_tip(tip_hash, &network, boundary)
            .is_some());
        assert!(
            cache
                .template_for_tip(tip_hash, &network, rollback)
                .is_none(),
            "every advertised timestamp must remain inside the local-clock bound on {network:?}",
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
