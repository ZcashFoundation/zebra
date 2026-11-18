//! Checkpoint queue admission under memory pressure.

use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn replacement_at_byte_limit_preserves_other_requests() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let (chain, checkpoints) = chain_with_checkpoints(8, &[0, 4, 8]);
    let state_service = MockService::build().for_unit_tests();
    let mut verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        state_service,
    )
    .map_err(|e| eyre!(e))?;

    let mut original = verifier.queue_block(chain[4].clone(), None)?;
    let mut unrelated = verifier.queue_block(chain[6].clone(), None)?;
    verifier.max_queued_bytes = verifier.queued_bytes;
    let replacement = Arc::new((*chain[4]).clone());
    let mut replacement_response = verifier.queue_block(replacement.clone(), None)?;

    assert!(matches!(
        original.try_recv(),
        Ok(Err(VerifyCheckpointError::NewerRequest { .. }))
    ));
    assert!(matches!(
        unrelated.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        replacement_response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(verifier.queued_bytes, verifier.max_queued_bytes);
    assert!(Arc::ptr_eq(
        &verifier.queued[&block::Height(4)][0].block.block,
        &replacement
    ));
    Ok(())
}

#[tokio::test]
async fn recovered_children_use_bounded_checkpoint_admission() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let (chain, checkpoints) = chain_with_checkpoints(8, &[0, 4, 8]);
    let state_service = MockService::build().for_unit_tests();
    let mut verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        state_service,
    )
    .map_err(|e| eyre!(e))?;

    let mut recovered = verifier.queue_block(chain[4].clone(), None)?;
    let child = verifier
        .queued
        .remove(&block::Height(4))
        .expect("the child was queued")
        .pop()
        .expect("the height contains the child");
    verifier.queued_bytes -= child.bytes;
    let mut later = verifier.queue_block(chain[6].clone(), None)?;
    verifier.max_queued_bytes = verifier.queued_bytes;
    verifier.requeue_child(child);
    assert!(verifier.queued_bytes <= verifier.max_queued_bytes);
    assert!(matches!(
        later.try_recv(),
        Ok(Err(VerifyCheckpointError::QueuedLimit))
    ));
    assert!(matches!(
        recovered.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(verifier.queued.contains_key(&block::Height(4)));

    Ok(())
}

#[tokio::test]
async fn recovery_tip_and_drop_release_queued_bodies() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let (chain, checkpoints) = chain_with_checkpoints(8, &[0, 4, 8]);
    let state_service = MockService::build().for_unit_tests();
    let mut verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        state_service,
    )
    .map_err(|e| eyre!(e))?;

    let earlier = Arc::new((*chain[4]).clone());
    let earlier_body = Arc::downgrade(&earlier);
    let mut earlier_response = verifier.queue_block(earlier, None)?;
    let later = Arc::new((*chain[6]).clone());
    let later_body = Arc::downgrade(&later);
    let mut later_response = verifier.queue_block(later, None)?;
    let later_bytes = verifier.queued[&block::Height(6)][0].bytes;

    verifier.reset_progress(Some(checkpoints[1]));
    assert_eq!(verifier.queued_bytes, later_bytes);
    assert_eq!(earlier_body.strong_count(), 0);
    assert!(!verifier.queued.contains_key(&block::Height(4)));
    assert!(matches!(
        earlier_response.try_recv(),
        Ok(Err(VerifyCheckpointError::AlreadyVerified { .. }))
    ));

    drop(verifier);
    assert_eq!(later_body.strong_count(), 0);
    assert!(matches!(
        later_response.try_recv(),
        Ok(Err(VerifyCheckpointError::Dropped))
    ));
    Ok(())
}

/// Cancelled responses and recovered children cannot free capacity held by commit owners.
#[tokio::test]
async fn checkpoint_commit_owners_stay_bounded_through_recovery() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        // Match the ordinary two-batch allowance without relying on new implementation fields,
        // so this consumer regression can also run against the pre-fix verifier.
        let owner_limit = 2_000;
        let last_height = u32::try_from(owner_limit).unwrap();
        let heights: Vec<_> = (0..=last_height).collect();
        let (chain, checkpoints) = chain_with_checkpoints(last_height, &heights);
        let extra_parent = checkpoints[owner_limit - 1].1;
        let bodies: Vec<_> = chain.iter().map(Arc::downgrade).collect();
        let (release, released) = tokio::sync::watch::channel(false);
        let attempts = Arc::new(AtomicUsize::new(0));
        let tip_error = Arc::new(AtomicBool::new(false));
        let state = tower::service_fn(move |request: zs::Request| {
            let mut released = released.clone();
            let attempts = attempts.clone();
            let tip_error = tip_error.clone();
            async move {
                match request {
                    zs::Request::CommitCheckpointVerifiedBlock(block) => {
                        if block.height == block::Height(0)
                            && attempts.fetch_add(1, Ordering::SeqCst) == 0
                        {
                            return Err::<zs::Response, BoxError>(Box::new(
                                zs::CommitCheckpointVerifiedError::from(
                                    zs::CommitBlockError::new_write_task_exited(),
                                ),
                            ));
                        }
                        released.wait_for(|released| *released).await.unwrap();
                        if block.height == block::Height(1) {
                            tip_error.store(true, Ordering::SeqCst);
                            Err("terminal commit failure".into())
                        } else {
                            Ok(zs::Response::Committed(block.hash))
                        }
                    }
                    zs::Request::Tip if tip_error.load(Ordering::SeqCst) => {
                        Err("terminal recovery tip failure".into())
                    }
                    zs::Request::Tip => Ok(zs::Response::Tip(None)),
                    _ => panic!("only checkpoint commits and recovery tips are expected"),
                }
            }
        });
        let mut verifier =
            CheckpointVerifier::from_list(checkpoints, &network_without_pow(), None, state)
                .unwrap();
        let mut accepted = 0;
        for block in chain {
            // Polling once then dropping models caller cancellation, not commit cancellation.
            match verifier.call(block).now_or_never() {
                None => accepted += 1,
                Some(Err(error)) => {
                    assert!(matches!(error, VerifyCheckpointError::QueuedLimit));
                    assert_eq!(error.misbehavior_score(), 0);
                }
                Some(Ok(_)) => panic!("state commits are still gated"),
            }
        }
        assert_eq!(
            accepted, owner_limit,
            "accepted owners must share one bound"
        );
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.strong_count() != 0)
                .count(),
            owner_limit,
            "verified bodies remain bounded after leaving the queue",
        );

        // Hold the unchecked child outside the queue before returning it through recovery.
        let failure = loop {
            match verifier.reset_receiver.try_recv() {
                Ok(failure) => break failure,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        assert!(failure.child.is_some());
        assert!(matches!(
            verifier
                .call(block_at(last_height, extra_parent))
                .now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ));
        verifier.reset_sender.send(failure).unwrap();
        assert!(matches!(
            verifier
                .call(block_at(last_height, extra_parent))
                .now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ));

        // Reset cannot free reservations owned by running commits either.
        verifier.reset_progress(None);
        assert!(matches!(
            verifier
                .call(block_at(last_height, extra_parent))
                .now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ));

        // Successful commits, a terminal commit error, and a failed tip lookup all release bodies.
        release.send(true).unwrap();
        while bodies.iter().any(|body| body.strong_count() != 0) {
            tokio::task::yield_now().await;
        }
        assert!(
            verifier
                .call(block_at(last_height, extra_parent))
                .now_or_never()
                .is_none(),
            "finished owners must return admission capacity",
        );
        drop(verifier);
    })
    .await
    .expect("cancelled commit owners must recover or finish without escaping their bound");
}
