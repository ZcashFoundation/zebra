//! Fixed test vectors for the precomputed block template cache.

use std::time::Duration;

use zcash_keys::address::Address;

use zebra_chain::{
    block,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
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

/// Returns a mock chain tip on the block `tip_hash`, and the sender that changes it.
///
/// Keep the sender until the test ends: once it is dropped, waiting for a chain tip change fails.
fn chain_tip_at(tip_hash: block::Hash) -> (MockChainTip, MockChainTipSender) {
    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    chain_tip_sender.send_best_tip_hash(tip_hash);

    (chain_tip, chain_tip_sender)
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

/// A refreshed template with the same transactions is validated as a proposal that has the
/// transaction objects of the last validated proposal. The block verifier compares a solved
/// block's transactions with its proposal's by pointer before it hashes them, and a solved block
/// gets the transaction objects of the first validated proposal with its transaction bytes.
#[tokio::test]
async fn refreshed_template_proposals_share_their_transactions() {
    let _init_guard = zebra_test::init();

    // The verifier accepts every proposal, and reports the blocks it gets.
    let (verified_tx, mut verified_rx) = tokio::sync::mpsc::unbounded_channel();
    let verifier = tower::service_fn(move |request: zebra_consensus::Request| {
        let block = request.block();
        let hash = block.hash();
        verified_tx
            .send(block)
            .expect("the test holds the receiver");
        std::future::ready(Ok::<_, zebra_consensus::BoxError>(hash))
    });

    let cache = TemplateCache::default();
    let (chain_tip, _chain_tip_sender) = chain_tip_at(template().previous_block_hash);
    let validator = tokio::spawn(validate_templates(
        cache.subscribe(),
        Network::Mainnet,
        verifier,
        ProposalTransactions::default(),
        chain_tip,
    ));

    let first_template = template();
    let mut refreshed_template = first_template.clone();
    refreshed_template.cur_time = DateTime32::from(first_template.cur_time.timestamp() + 1);

    let mut proposals = Vec::new();
    for template in [first_template, refreshed_template] {
        cache.publish(template);

        let proposal = tokio::time::timeout(Duration::from_secs(10), verified_rx.recv())
            .await
            .expect("each published template is validated")
            .expect("the validator holds the sender");
        proposals.push(proposal);
    }
    validator.abort();

    assert_ne!(proposals[0].header.time, proposals[1].header.time);
    assert!(
        proposals[0]
            .transactions
            .iter()
            .zip(&proposals[1].transactions)
            .all(|(first, refreshed)| Arc::ptr_eq(first, refreshed)),
        "a refreshed proposal reuses the transactions of the proposal it refreshes",
    );
}

/// Returns a block verifier that reports the proposals it gets, and never answers them.
fn stuck_verifier() -> (
    impl BlockVerifierService,
    tokio::sync::mpsc::UnboundedReceiver<Arc<block::Block>>,
) {
    let (verified_tx, verified_rx) = tokio::sync::mpsc::unbounded_channel();
    let verifier = tower::service_fn(move |request: zebra_consensus::Request| {
        verified_tx
            .send(request.block())
            .expect("the test holds the receiver");
        std::future::pending::<Result<block::Hash, zebra_consensus::BoxError>>()
    });

    (verifier, verified_rx)
}

/// A template on another block replaces a validation that is still running, so a validation that
/// is stuck on the old chain tip doesn't hold up the templates on the new one.
#[tokio::test]
async fn template_on_another_block_replaces_a_running_validation() {
    let _init_guard = zebra_test::init();

    let (verifier, mut verified_rx) = stuck_verifier();
    let cache = TemplateCache::default();
    let (chain_tip, chain_tip_sender) = MockChainTip::new();
    let validator = tokio::spawn(validate_templates(
        cache.subscribe(),
        Network::Mainnet,
        verifier,
        ProposalTransactions::default(),
        chain_tip,
    ));

    let first_template = template();
    let mut next_tip_template = first_template.clone();
    next_tip_template.previous_block_hash = block::Hash([0xcd; 32]);

    let mut proposals = Vec::new();
    for template in [first_template, next_tip_template] {
        // Like the updater, publish a template on the chain tip after the tip changes.
        chain_tip_sender.send_best_tip_hash(template.previous_block_hash);
        cache.publish(template);

        // The verifier never answers, so the second proposal only reaches it if the first
        // validation was dropped, well before the validation timeout.
        let proposal = tokio::time::timeout(TEMPLATE_VALIDATION_TIMEOUT / 3, verified_rx.recv())
            .await
            .expect("a template on another block is validated without waiting")
            .expect("the validator holds the sender");
        proposals.push(proposal);
    }
    validator.abort();

    assert_ne!(
        proposals[0].header.previous_block_hash,
        proposals[1].header.previous_block_hash
    );
}

/// A refreshed template on the same block waits for the running validation, which is dropped
/// after the validation timeout.
#[tokio::test(start_paused = true)]
async fn stuck_validation_is_dropped_after_its_timeout() {
    let _init_guard = zebra_test::init();

    let (verifier, mut verified_rx) = stuck_verifier();
    let cache = TemplateCache::default();
    let (chain_tip, _chain_tip_sender) = chain_tip_at(template().previous_block_hash);
    let validator = tokio::spawn(validate_templates(
        cache.subscribe(),
        Network::Mainnet,
        verifier,
        ProposalTransactions::default(),
        chain_tip,
    ));

    let first_template = template();
    let mut refreshed_template = first_template.clone();
    refreshed_template.cur_time = DateTime32::from(first_template.cur_time.timestamp() + 1);

    cache.publish(first_template);
    let first = verified_rx
        .recv()
        .await
        .expect("the validator holds the sender");
    let first_validation_started = tokio::time::Instant::now();

    // The clock is paused, and only moves when every task is waiting. So the refreshed proposal
    // reaches the verifier at the time the first validation is dropped.
    cache.publish(refreshed_template);
    let refreshed = tokio::time::timeout(TEMPLATE_VALIDATION_TIMEOUT * 2, verified_rx.recv())
        .await
        .expect("a stuck validation is dropped after its timeout")
        .expect("the validator holds the sender");
    validator.abort();

    assert_ne!(first.header.time, refreshed.header.time);
    assert_eq!(
        first.header.previous_block_hash,
        refreshed.header.previous_block_hash
    );
    assert!(
        first_validation_started.elapsed() >= TEMPLATE_VALIDATION_TIMEOUT - Duration::from_secs(1),
        "a refreshed template on the same block waits for the running validation",
    );
}

/// Returns a block verifier that, like the state, only accepts a proposal on the block that
/// `chain_tip` reports, and reports the proposals it accepts.
fn tip_checking_verifier(
    chain_tip: MockChainTip,
) -> (
    impl BlockVerifierService,
    tokio::sync::mpsc::UnboundedReceiver<Arc<block::Block>>,
) {
    let (accepted_tx, accepted_rx) = tokio::sync::mpsc::unbounded_channel();
    let verifier = tower::service_fn(move |request: zebra_consensus::Request| {
        let block = request.block();
        let result: Result<block::Hash, zebra_consensus::BoxError> =
            if chain_tip.best_tip_hash() == Some(block.header.previous_block_hash) {
                let hash = block.hash();
                accepted_tx
                    .send(block)
                    .expect("the test holds the receiver");
                Ok(hash)
            } else {
                Err("proposal is not based on the current best chain tip".into())
            };

        std::future::ready(result)
    });

    (verifier, accepted_rx)
}

/// A template on a block that `latest_chain_tip` doesn't report yet is validated once it does,
/// because the state rejects a proposal that isn't on its tip. On an empty mempool, the next
/// template on the block can have the same long poll ID, so a long polling miner keeps working on
/// the first template, and submits it when it solves it.
#[tokio::test(start_paused = true)]
async fn template_on_a_block_the_chain_tip_doesnt_report_yet_is_validated_once_it_does() {
    let _init_guard = zebra_test::init();

    let first_template = template();
    let mut next_template = first_template.clone();
    next_template.cur_time = DateTime32::from(first_template.cur_time.timestamp() + 1);

    // The chain tip is the parent of the block that the templates are on.
    let (chain_tip, chain_tip_sender) = chain_tip_at(block::Hash([0x01; 32]));
    let (verifier, mut accepted_rx) = tip_checking_verifier(chain_tip.clone());
    let cache = TemplateCache::default();
    let validator = tokio::spawn(validate_templates(
        cache.subscribe(),
        Network::Mainnet,
        verifier,
        ProposalTransactions::default(),
        chain_tip,
    ));

    cache.publish(first_template.clone());

    // The clock is paused, and only moves when every task is waiting. So the validator has taken
    // the first template when this sleep ends.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The chain tip reports the block, and then the updater publishes the next template on it.
    chain_tip_sender.send_best_tip_hash(first_template.previous_block_hash);
    cache.publish(next_template.clone());

    let mut validated_times = Vec::new();
    for _ in 0..2 {
        let proposal = tokio::time::timeout(Duration::from_secs(10), accepted_rx.recv())
            .await
            .expect("both templates are validated")
            .expect("the validator holds the sender");
        validated_times.push(proposal.header.time);
    }
    validator.abort();

    assert_eq!(
        validated_times,
        [first_template.cur_time, next_template.cur_time]
            .map(chrono::DateTime::<chrono::Utc>::from),
        "the first template is validated first, and then the next one",
    );
}

/// A template on another block ends the wait for the chain tip, so a template on a block that
/// doesn't become the chain tip doesn't hold up the chain tip's template.
#[tokio::test(start_paused = true)]
async fn template_on_another_block_ends_the_wait_for_the_chain_tip() {
    let _init_guard = zebra_test::init();

    let other_block_template = template();
    let mut tip_template = other_block_template.clone();
    tip_template.previous_block_hash = block::Hash([0x01; 32]);

    let (chain_tip, _chain_tip_sender) = chain_tip_at(tip_template.previous_block_hash);
    let (verifier, mut accepted_rx) = tip_checking_verifier(chain_tip.clone());
    let cache = TemplateCache::default();
    let validator = tokio::spawn(validate_templates(
        cache.subscribe(),
        Network::Mainnet,
        verifier,
        ProposalTransactions::default(),
        chain_tip,
    ));

    cache.publish(other_block_template);

    // The clock is paused, so the validator is waiting for the block when this sleep ends.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The block doesn't become the chain tip, and the updater publishes a template on the tip.
    cache.publish(tip_template.clone());

    // The block is never the chain tip, so the proposal on the tip only reaches the verifier if
    // the wait for the block was dropped, well before the validation timeout.
    let proposal = tokio::time::timeout(TEMPLATE_VALIDATION_TIMEOUT / 3, accepted_rx.recv())
        .await
        .expect("a template on another block is validated without waiting")
        .expect("the validator holds the sender");
    validator.abort();

    assert_eq!(
        proposal.header.previous_block_hash,
        tip_template.previous_block_hash
    );
}

/// A template on a block that the state was still verifying is kept while the chain tip is the
/// block's parent, dropped when the chain tip is another block, and published once the chain tip
/// is the block, unless the cache already has a template on the block.
#[test]
fn early_template_is_kept_dropped_or_published_by_the_chain_tip() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let template = template();
    let block_hash = template.previous_block_hash;
    let parent_hash = block::Hash([0x01; 32]);
    let other_block_hash = block::Hash([0x02; 32]);
    let early_template = || {
        Some(EarlyTemplate {
            parent_hash: Some(parent_hash),
            template: template.clone(),
        })
    };
    let cached_time = |cache: &TemplateCache| {
        cache
            .template_for_tip(block_hash, &network, template.max_time)
            .map(|template| template.cur_time)
    };

    let cache = TemplateCache::default();
    let mut kept = early_template();
    publish_early_template(&mut kept, Some(parent_hash), &cache);
    assert!(
        kept.is_some(),
        "the template is kept while its block can still be committed"
    );
    assert!(cache.is_empty(), "the template isn't published");

    publish_early_template(&mut kept, Some(other_block_hash), &cache);
    assert!(
        kept.is_none(),
        "the template is dropped when another block is the chain tip"
    );
    assert!(cache.is_empty(), "the template isn't published");

    let mut kept = early_template();
    publish_early_template(&mut kept, Some(block_hash), &cache);
    assert!(kept.is_none(), "a published template isn't kept");
    assert_eq!(
        cached_time(&cache),
        Some(template.cur_time),
        "the template is published once its block is the chain tip",
    );

    // A template with mempool transactions on the block can be built before this task sees the
    // block become the chain tip.
    let mut full_template = template.clone();
    full_template.cur_time = DateTime32::from(template.cur_time.timestamp() + 1);
    cache.publish(full_template.clone());

    let mut kept = early_template();
    publish_early_template(&mut kept, Some(block_hash), &cache);
    assert!(
        kept.is_none(),
        "the template isn't kept once its block is the chain tip"
    );
    assert_eq!(
        cached_time(&cache),
        Some(full_template.cur_time),
        "the template doesn't replace a template the cache already has on the block",
    );
}
