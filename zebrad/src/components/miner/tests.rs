//! Internal miner eligibility and notification-race regressions.

use super::*;

#[tokio::test(start_paused = true)]
async fn parked_solver_rechecks_parent_without_new_work() {
    let (sender, receiver) =
        watch::channel(Some(zebra_chain::block::genesis::regtest_genesis_block()));
    let mut receiver = WatchReceiver::new(receiver);
    receiver.mark_as_seen();

    // Retained work is not republished when its parent commits, so the wait must end anyway.
    let start = tokio::time::Instant::now();
    wait_for_committed_parent(&mut receiver)
        .await
        .expect("the template sender is still open");
    assert_eq!(start.elapsed(), BLOCK_TEMPLATE_REFRESH_LIMIT);

    drop(sender);
    assert!(wait_for_committed_parent(&mut receiver).await.is_err());
}
