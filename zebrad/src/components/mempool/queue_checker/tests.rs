//! Private mining-parent notifications must drive the canonical mempool owner immediately.

use tokio::{sync::watch, time::Instant};
use zebra_chain::parameters::Network;
use zebra_state::ChainTipSender;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::*;

#[tokio::test(start_paused = true)]
async fn equal_parent_change_during_queue_check_is_not_lost_or_rate_limited() {
    let _init_guard = zebra_test::init();
    let mut mempool: MockService<mempool::Request, mempool::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let (_committed_tip, latest_chain_tip, _) = ChainTipSender::new(None, &Network::Mainnet);
    let (mining_tip, receiver) = watch::channel(None);
    let checker = QueueChecker::spawn(
        mempool.clone(),
        Arc::new(Notify::new()),
        Arc::default(),
        latest_chain_tip,
        zebra_state::MiningTipChange { receiver },
    );
    let started = Instant::now();
    let first = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::CheckForVerifiedTransactions)
        })
        .await;
    // An invalidation can leave the parent unchanged, and arrive while the owner is polling.
    mining_tip.send_replace(None);
    first.respond(mempool::Response::CheckedForVerifiedTransactions);
    let changed = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::CheckForVerifiedTransactions)
        })
        .await;
    assert_eq!(
        Instant::now(),
        started,
        "the five-second backstop must not drive this check"
    );
    changed.respond(mempool::Response::CheckedForVerifiedTransactions);
    drop(mining_tip);
    checker.await.unwrap().unwrap();
}
