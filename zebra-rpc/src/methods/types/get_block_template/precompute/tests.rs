//! Fixed test vectors for the block template updater's wait phase.
//!
//! [`wait_for_change`] decides whether a mempool change is worth rebuilding the template for, and
//! debounces the ones that are. Every decision shows up as *when* it returns: after
//! [`MEMPOOL_DEBOUNCE`] if a change woke it, or after [`BACKSTOP_REFRESH`] if nothing did. These
//! tests run on a paused clock and assert on that duration, so each costs no real time and says
//! which of the two happened, instead of waiting to see whether a rebuild arrives.

use std::{collections::HashSet, time::Duration};

use tokio::{sync::broadcast, time::Instant};

use zcash_keys::address::Address;

use zebra_chain::{
    block,
    chain_tip::mock::MockChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    transaction::{self, UnminedTxId},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
};
use zebra_node_services::{mempool, BoxError};
use zebra_state::GetBlockTemplateChainInfo;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    config::mining::{default_miner_address, MinerAddressType},
    methods::tests::utils::fake_history_tree,
};

use super::*;

/// A [`MockService`] standing in for the mempool.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

/// Returns a mempool mock whose request deadline outlasts [`BACKSTOP_REFRESH`].
fn mock_mempool() -> MockMempool {
    MockService::build()
        .with_max_request_delay(BACKSTOP_REFRESH * 2)
        .for_unit_tests()
}

/// Returns a transaction ID derived from `byte`.
fn tx_id(byte: u8) -> UnminedTxId {
    UnminedTxId::from_legacy_id(transaction::Hash([byte; 32]))
}

/// Returns a template with an explicit timestamp boundary.
fn template_with_max_time(net: &Network, max_time: DateTime32) -> BlockTemplateResponse {
    template_with_ids(net, max_time, &HashSet::new())
}

/// Returns a template whose long-poll ID covers `mempool_tx_ids`.
fn template(mempool_tx_ids: &HashSet<UnminedTxId>) -> BlockTemplateResponse {
    template_with_ids(
        &Network::Mainnet,
        DateTime32::from(1654008719),
        mempool_tx_ids,
    )
}

fn template_with_ids(
    net: &Network,
    max_time: DateTime32,
    mempool_tx_ids: &HashSet<UnminedTxId>,
) -> BlockTemplateResponse {
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
        mempool_tx_ids.iter().copied(),
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
    cache.publish(current, Default::default());
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
    cache.publish(median_capped, Default::default());
    let rollback = max_time
        .saturating_sub(Duration32::from_hours(2))
        .saturating_sub(Duration32::from_seconds(1));
    let narrowed = cache
        .template_for_tip(tip_hash, &network, rollback)
        .unwrap();
    assert!(narrowed.max_time < max_time);
    // A served copy is now abbreviated, but the cache retains the true median-time cap.
    assert!(
        cache
            .template_for_tip(tip_hash, &network, after_max_time)
            .is_some(),
        "rebuilding cannot change difficulty beyond the median-time cap",
    );

    let mut minimum_difficulty = template_with_max_time(&network, max_time);
    minimum_difficulty.bits = network.target_difficulty_limit().to_compact();
    minimum_difficulty.target = network.target_difficulty_limit();
    cache.publish(minimum_difficulty, Default::default());
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
        cache.publish(
            template_with_max_time(&network, max_time),
            Default::default(),
        );
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
            cache.publish(current, Default::default());
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

/// A clock rollback narrows only the served copy, including its current time and work identity.
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
        current.submit_old = Some(true);
        let tip_hash = current.previous_block_hash;
        let boundary = current.max_time.saturating_sub(Duration32::from_hours(2));
        let rollback = boundary.saturating_sub(Duration32::from_seconds(1));
        // Checking only cur_time would miss the invalid advertised maximum.
        assert!(current.cur_time <= rollback.saturating_add(Duration32::from_hours(2)));
        cache.publish(current.clone(), Default::default());

        assert!(cache
            .template_for_tip(tip_hash, &network, boundary)
            .is_some());
        let served = cache
            .template_for_tip(tip_hash, &network, rollback)
            .expect("a nonempty narrowed range can be served immediately");
        assert_eq!(
            served.max_time,
            current.max_time.saturating_sub(Duration32::from_seconds(1))
        );
        assert_eq!(served.cur_time, current.cur_time);
        assert_ne!(served.long_poll_id, current.long_poll_id);
        assert!(!served.long_poll_id.submit_old(&current.long_poll_id));
        assert_eq!(served.submit_old, Some(false));
        assert_eq!(served.coinbase_txn, current.coinbase_txn);
        // On-demand construction uses the same boundary after its coinbase proof completes.
        let mut completed = current.clone();
        completed.clamp_time_range(rollback).unwrap();
        assert_eq!(&completed, served.as_ref());
        assert_eq!(served.bits, current.bits);
        assert_eq!(
            cache
                .template_for_tip(tip_hash, &network, boundary)
                .as_deref(),
            Some(&current)
        );

        // A larger rollback can require reducing cur_time, but never below min_time.
        let at_minimum = current.min_time.saturating_sub(Duration32::from_hours(2));
        let served = cache
            .template_for_tip(tip_hash, &network, at_minimum)
            .unwrap();
        assert_eq!(served.cur_time, current.min_time);
        assert_eq!(served.max_time, current.min_time);
        assert!(
            cache
                .template_for_tip(
                    tip_hash,
                    &network,
                    at_minimum.saturating_sub(Duration32::from_seconds(1)),
                )
                .is_none(),
            "an empty range must not be fabricated"
        );

        for (min_time, cur_time, max_time) in [
            (current.cur_time, current.min_time, current.max_time),
            (current.min_time, current.max_time, current.cur_time),
            (current.max_time, current.cur_time, current.min_time),
        ] {
            let mut invalid = current.clone();
            invalid.min_time = min_time;
            invalid.cur_time = cur_time;
            invalid.max_time = max_time;
            cache.publish(invalid, Default::default());
            assert!(cache
                .template_for_tip(tip_hash, &network, boundary)
                .is_none());
        }
    }
}

/// Returns a cache holding a template built from `mempool_tx_ids`.
fn cache_built_from(mempool_tx_ids: HashSet<UnminedTxId>) -> TemplateCache {
    let cache = TemplateCache::default();
    cache.publish(template(&mempool_tx_ids), mempool_tx_ids);

    cache
}

/// Runs one wait phase and returns whether it asks for a rebuild, and how long it waited.
///
/// The clock is paused, so the duration is exactly the deadline the wait ended on, which separates
/// a change waking the updater from the backstop expiring.
async fn wait_once(
    cache: &TemplateCache,
    mempool_changes: &mut broadcast::Receiver<MempoolChange>,
    mempool: &MockMempool,
) -> (bool, Duration) {
    // A tip that never changes, so only the mempool paths are under test.
    let (tip, _tip_sender) = MockChainTip::new();
    let started = Instant::now();
    let rebuild = wait_for_change(&tip, mempool_changes, cache, mempool).await;

    (rebuild, started.elapsed())
}

/// Checks that a burst of additions costs one wait, and that the wait consumes the whole burst.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_burst_of_additions_is_one_debounced_wait() {
    let _init_guard = zebra_test::init();

    let (change_sender, mut change_receiver) = broadcast::channel(200);
    let mempool = mock_mempool();
    let cache = cache_built_from(HashSet::new());

    for byte in 0..20 {
        change_sender
            .send(MempoolChange::added([tx_id(byte)].into_iter().collect()))
            .expect("the receiver below is open");
    }

    let (rebuild, waited) = wait_once(&cache, &mut change_receiver, &mempool).await;

    assert!(rebuild, "an addition should ask for a rebuild");
    assert_eq!(
        waited, MEMPOOL_DEBOUNCE,
        "the wait should end one debounce after the first addition, neither immediately nor at \
         the backstop",
    );
    assert!(
        change_receiver.try_recv().is_err(),
        "the debounce should consume the rest of the burst, so the next wait doesn't rebuild \
         again for changes this one already covered",
    );
}

/// Checks that invalidating transactions the template was not built from doesn't wake the wait.
///
/// The mempool sends `Invalidated` for transactions that failed verification and were never in the
/// mempool, so rebuilding for those would let a peer spend Zebra's CPU by sending invalid
/// transactions.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn invalidating_unused_transactions_waits_for_the_backstop() {
    let _init_guard = zebra_test::init();

    let (change_sender, mut change_receiver) = broadcast::channel(200);
    let mempool = mock_mempool();
    let cache = cache_built_from([tx_id(1)].into_iter().collect());

    for byte in 100..120 {
        change_sender
            .send(MempoolChange::invalidated(
                [tx_id(byte)].into_iter().collect(),
            ))
            .expect("the receiver below is open");
    }

    let (rebuild, waited) = wait_once(&cache, &mut change_receiver, &mempool).await;

    assert!(rebuild, "the backstop should ask for a rebuild");
    assert_eq!(
        waited, BACKSTOP_REFRESH,
        "invalidating transactions the template wasn't built from should leave the wait to the \
         backstop",
    );
}

/// Checks that invalidating a transaction the template's long poll ID covers wakes the wait, even
/// though ZIP-317 left it out of the template.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn invalidating_an_unselected_transaction_ends_the_wait() {
    let _init_guard = zebra_test::init();

    let (change_sender, mut change_receiver) = broadcast::channel(200);
    let mempool = mock_mempool();

    // The template holds no transactions, so this ID is in the mempool the template was built
    // from without being in the template.
    let unselected = tx_id(1);
    let cache = cache_built_from([unselected].into_iter().collect());

    change_sender
        .send(MempoolChange::invalidated(
            [unselected].into_iter().collect(),
        ))
        .expect("the receiver below is open");

    let (rebuild, waited) = wait_once(&cache, &mut change_receiver, &mempool).await;

    assert!(
        rebuild,
        "the invalidated transaction should ask for a rebuild"
    );
    assert_eq!(
        waited, MEMPOOL_DEBOUNCE,
        "a transaction the template's long poll ID covers should end the wait even when ZIP-317 \
         didn't select it, because the mempool the template was built from no longer exists",
    );
}

/// Checks that overflowing the change channel doesn't wake the wait while the mempool still holds
/// what the template was built from.
///
/// Waking on a lagged channel would undo the filter the other tests cover: a peer sending invalid
/// transactions fast enough makes the channel lag, and every overflow would buy the rebuild its
/// rejected transactions could not.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_lagging_channel_with_an_unchanged_mempool_waits_for_the_backstop() {
    let _init_guard = zebra_test::init();

    let capacity = 4;
    let (change_sender, mut change_receiver) = broadcast::channel(capacity);
    let template_tx_ids: HashSet<UnminedTxId> = [tx_id(1)].into_iter().collect();
    let cache = cache_built_from(template_tx_ids.clone());

    // The wait and the responder have to share one mock: a `MockService` clone only sees requests
    // made after it was cloned, and answering on an unrelated instance would leave the request
    // unanswered, which takes the same branch as an unchanged mempool and proves nothing.
    let mut responding_mempool = mock_mempool();
    let mempool = responding_mempool.clone();

    for byte in 0..(capacity as u8 + 2) {
        change_sender
            .send(MempoolChange::invalidated(
                [tx_id(byte)].into_iter().collect(),
            ))
            .expect("the receiver below is open");
    }

    // The same mempool the template was built from, so there is nothing to rebuild for.
    let responder = tokio::spawn(async move {
        responding_mempool
            .expect_request(mempool::Request::TransactionIds)
            .await
            .respond(mempool::Response::TransactionIds(template_tx_ids));
    });

    let (rebuild, waited) = wait_once(&cache, &mut change_receiver, &mempool).await;

    // Joining the responder is what keeps this test honest: it panics if the lagged channel never
    // made the wait ask the mempool what it holds.
    responder
        .await
        .expect("the lagged wait should request the mempool's transaction IDs");

    assert!(rebuild, "the backstop should ask for a rebuild");
    assert_eq!(
        waited, BACKSTOP_REFRESH,
        "a lagging channel should leave the wait to the backstop while the mempool still holds \
         what the template was built from",
    );
}

/// Checks that overflowing the change channel does wake the wait when the mempool no longer holds
/// what the template was built from, which is the case the comparison above exists to allow.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_lagging_channel_with_a_changed_mempool_ends_the_wait() {
    let _init_guard = zebra_test::init();

    let capacity = 4;
    let (change_sender, mut change_receiver) = broadcast::channel(capacity);
    let cache = cache_built_from([tx_id(1)].into_iter().collect());

    let mut responding_mempool = mock_mempool();
    let mempool = responding_mempool.clone();

    for byte in 0..(capacity as u8 + 2) {
        change_sender
            .send(MempoolChange::invalidated(
                [tx_id(byte)].into_iter().collect(),
            ))
            .expect("the receiver below is open");
    }

    // A different mempool from the one the template was built from.
    let responder = tokio::spawn(async move {
        responding_mempool
            .expect_request(mempool::Request::TransactionIds)
            .await
            .respond(mempool::Response::TransactionIds(
                [tx_id(2)].into_iter().collect(),
            ));
    });

    let (rebuild, waited) = wait_once(&cache, &mut change_receiver, &mempool).await;

    responder
        .await
        .expect("the lagged wait should request the mempool's transaction IDs");

    assert!(rebuild, "the changed mempool should ask for a rebuild");
    assert_eq!(
        waited, MEMPOOL_DEBOUNCE,
        "a lagging channel over a mempool that no longer holds what the template was built from \
         should end the wait",
    );
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

    cache.publish(template(&HashSet::new()), HashSet::new());

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
        cache.publish(template(&HashSet::new()), HashSet::new());

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
    let template = template(&HashSet::new());
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
    let height = Height(template(&HashSet::new()).height);
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
