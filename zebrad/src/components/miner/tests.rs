//! Controlled-channel tests for solver work changes.

use super::*;
use zebra_chain::serialization::ZcashDeserializeInto;

fn template() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vector is a valid block")
}

#[tokio::test(start_paused = true)]
async fn initial_template_wakes_solver_without_retry_delay() {
    let (sender, receiver) = watch::channel(None);
    let mut receiver = WatchReceiver::new(receiver);
    let start = tokio::time::Instant::now();
    let waiter = tokio::spawn(async move {
        wait_for_mining_template_change(&mut receiver).await;
        receiver.cloned_watch_data()
    });
    tokio::task::yield_now().await;
    sender.send(Some(template())).unwrap();
    assert!(waiter.await.unwrap().is_some());
    assert_eq!(tokio::time::Instant::now(), start);
}

#[tokio::test(start_paused = true)]
async fn parent_change_interrupts_refresh_and_retry_cooldowns() {
    for delay in [BLOCK_TEMPLATE_REFRESH_LIMIT, BLOCK_TEMPLATE_WAIT_TIME] {
        let (sender, mut receiver) = watch::channel(Some((block::Height(1), block::Hash([1; 32]))));
        let start = tokio::time::Instant::now();
        let waiter =
            tokio::spawn(async move { wait_for_mining_tip_change(&mut receiver, delay).await });
        tokio::task::yield_now().await;
        sender
            .send(Some((block::Height(2), block::Hash([2; 32]))))
            .unwrap();
        assert!(waiter.await.unwrap().unwrap());
        assert_eq!(tokio::time::Instant::now(), start);
    }
}

#[tokio::test(start_paused = true)]
async fn unchanged_parent_retains_refresh_rate_limit() {
    let (_sender, mut receiver) = watch::channel(None);
    let start = tokio::time::Instant::now();
    assert!(
        !wait_for_mining_tip_change(&mut receiver, BLOCK_TEMPLATE_REFRESH_LIMIT)
            .await
            .unwrap()
    );
    assert_eq!(
        tokio::time::Instant::now() - start,
        BLOCK_TEMPLATE_REFRESH_LIMIT
    );
}

#[tokio::test]
async fn solved_work_is_rechecked_after_the_last_solver_cancellation_check() {
    let block = template();
    let old_header = *block.header;
    let (template_sender, receiver) = watch::channel(Some(block));
    let receiver = WatchReceiver::new(receiver);
    let (tip_sender, mining_tip) =
        watch::channel(Some((block::Height(0), old_header.previous_block_hash)));
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header).is_ok());

    // Release a controlled solver result only after its parent has been replaced.
    let (solution_sender, solution_receiver) = tokio::sync::oneshot::channel();
    let submission = tokio::spawn(async move {
        solution_receiver.await.unwrap();
        cancel_if_mining_template_changed(&receiver, &mining_tip, old_header)
    });
    tip_sender
        .send(Some((block::Height(1), block::Hash([9; 32]))))
        .unwrap();
    solution_sender.send(()).unwrap();
    assert!(submission.await.unwrap().is_err());
    drop(template_sender);
}

#[test]
fn compatible_mempool_work_survives_but_replacement_and_withdrawal_cancel() {
    let old_block = template();
    let old_header = *old_block.header;
    let mut new_header = old_header;
    new_header.nonce[0] ^= 1;
    assert!(!should_replace_mining_template(
        Some(old_header),
        new_header,
        Some(true)
    ));
    assert!(should_replace_mining_template(
        Some(old_header),
        new_header,
        Some(false)
    ));
    assert!(should_replace_mining_template(None, new_header, Some(true)));

    let (sender, receiver) = watch::channel(Some(old_block.clone()));
    let receiver = WatchReceiver::new(receiver);
    let (tip_sender, mining_tip) =
        watch::channel(Some((block::Height(0), old_header.previous_block_hash)));
    sender.send(Some(old_block.clone())).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header).is_ok());
    let mut replacement = (*old_block).clone();
    replacement.header = Arc::new(new_header);
    sender.send(Some(Arc::new(replacement))).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header).is_err());
    sender.send(None).unwrap();
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header).is_err());
    sender.send(Some(old_block)).unwrap();
    drop(tip_sender);
    assert!(cancel_if_mining_template_changed(&receiver, &mining_tip, old_header).is_err());
}
