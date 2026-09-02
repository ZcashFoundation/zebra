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
    serialization::DateTime32,
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
///
/// On a paused clock an unanswered request would otherwise become the earliest deadline, and the
/// mock would panic before the wait reached the backstop.
fn mock_mempool() -> MockMempool {
    MockService::build()
        .with_max_request_delay(BACKSTOP_REFRESH * 2)
        .for_unit_tests()
}

/// Returns a transaction ID derived from `byte`, so tests can name IDs without building
/// transactions.
fn tx_id(byte: u8) -> UnminedTxId {
    UnminedTxId::from_legacy_id(transaction::Hash([byte; 32]))
}

/// Returns a template whose long poll ID covers `mempool_tx_ids`.
///
/// The template itself contains no transactions, so every ID here is one the template's long poll
/// ID covers without being in the template: the difference one of the tests below covers.
fn template(mempool_tx_ids: &HashSet<UnminedTxId>) -> BlockTemplateResponse {
    let net = Network::Mainnet;
    let tip_height = NetworkUpgrade::Nu5
        .activation_height(&net)
        .expect("Nu5 is active on Mainnet");

    let miner_params = MinerParams::from(
        Address::decode(
            &net,
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
        max_time: DateTime32::from(1654008719),
        chain_history_root: fake_history_tree(&net).hash(),
    };

    let long_poll_id = LongPollInput::new(
        chain_info.tip_height,
        chain_info.tip_hash,
        chain_info.max_time,
        mempool_tx_ids.iter().copied(),
    )
    .generate_id();

    BlockTemplateResponse::new_internal(
        &net,
        &CoinbaseCache::default(),
        &miner_params,
        &chain_info,
        long_poll_id,
        vec![],
        None,
    )
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
