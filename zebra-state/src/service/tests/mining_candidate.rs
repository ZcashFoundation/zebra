//! Failed candidate computations release admission without waiting for the verifier's guard.

use std::{sync::Arc, time::Duration};

use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use zebra_chain::{block::Block, transparent};

use super::mining_rejection::{candidate, state, subscribe};
use crate::{
    service::StateService, ReadRequest, ReadResponse, ReadStateService, Request, Response,
};

async fn assert_unpublished_candidate_releases_slot(
    state: &mut StateService,
    read: &ReadStateService,
    rejected: Arc<Block>,
    next: Arc<Block>,
) {
    let mut changes = subscribe(read).await;
    let Response::MiningCandidate(Some(mut guard)) = state
        .ready()
        .await
        .unwrap()
        .call(Request::MiningCandidate(rejected))
        .await
        .unwrap()
    else {
        panic!("the failed candidate initially reserves the computation slot");
    };
    guard
        .0
        .wait_for(Option::is_none)
        .await
        .expect("a completed computation without context releases its admission");
    assert!(!changes.0.has_changed().unwrap());
    assert_eq!(
        read.clone()
            .oneshot(ReadRequest::MiningCandidate)
            .await
            .unwrap(),
        ReadResponse::MiningCandidate(None)
    );

    let Response::MiningCandidate(Some(next_guard)) = state
        .ready()
        .await
        .unwrap()
        .call(Request::MiningCandidate(next.clone()))
        .await
        .unwrap()
    else {
        panic!("the next candidate is admitted while the failed candidate's guard is retained");
    };
    changes.changed().await;
    assert!(changes.is_current(next.hash()));
    assert!(guard.0.borrow().is_none());
    drop((guard, next_guard));
}

#[tokio::test(flavor = "multi_thread")]
async fn orphaned_mining_candidate_releases_slot_while_guard_is_retained() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let next = candidate(&read, false, 0).await;
        let mut orphan = (*next).clone();
        Arc::make_mut(&mut orphan.header).previous_block_hash = [0xff; 32].into();
        assert_ne!(
            orphan.header.previous_block_hash,
            read.best_tip().unwrap().1
        );
        assert_unpublished_candidate_releases_slot(&mut state, &read, Arc::new(orphan), next).await;
    })
    .await
    .expect("an unavailable parent must not reserve admission until its verifier finishes");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_mining_candidate_body_releases_slot_while_guard_is_retained() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let next = candidate(&read, false, 0).await;
        let original = candidate(&read, true, 0).await;
        let mut invalid = (*original).clone();
        let transaction = &original.transactions[1];
        let mut inputs = transaction.inputs();
        let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
            panic!("the fixture's non-coinbase transaction spends a transparent input");
        };
        *unlock_script = transparent::Script::new(&[0x52]);
        invalid.transactions[1] = Arc::new((**transaction).clone().with_transparent_inputs(inputs));
        assert_eq!(invalid.hash(), original.hash());
        assert_eq!(invalid.transactions[1].hash(), transaction.hash());
        assert_ne!(invalid.auth_data_root(), original.auth_data_root());
        assert_unpublished_candidate_releases_slot(&mut state, &read, Arc::new(invalid), next)
            .await;
    })
    .await
    .expect("an invalid body must not reserve admission until its verifier finishes");
}

/// Without a commit, rejection, or tip change, a valid candidate lives until its lifetime ends.
#[tokio::test(flavor = "multi_thread")]
async fn mining_candidate_expires_after_its_lifetime() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(true).await;
        let lifetime = Duration::from_millis(200);
        state.mining_candidate_lifetime = lifetime;
        let block = candidate(&read, false, 0).await;
        let changes = subscribe(&read).await;
        let admitted = tokio::time::Instant::now();
        let Response::MiningCandidate(Some(mut guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::MiningCandidate(block))
            .await
            .unwrap()
        else {
            panic!("the first candidate reserves the computation slot");
        };
        // Publication ends at the deadline whether or not the computation finished first.
        while guard.0.changed().await.is_ok() {}
        assert!(
            admitted.elapsed() >= lifetime,
            "a valid candidate must stay live until its lifetime ends"
        );
        assert!(changes.0.borrow().is_none());
        assert_eq!(
            read.oneshot(ReadRequest::MiningCandidate).await.unwrap(),
            ReadResponse::MiningCandidate(None)
        );
    })
    .await
    .expect("an expired candidate must be withdrawn without a commit or failure");
}

#[test]
fn timed_out_mining_candidate_keeps_queued_computation_bounded() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), async {
            // A checkpointed parent avoids `block_in_place` commits, so this test can pause time.
            let (mut state, read) = state(true).await;
            let block = candidate(&read, false, 0).await;
            let mut changes = subscribe(&read).await;
            let (release, released) = oneshot::channel::<()>();
            let (started, running) = oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                // Dropping release after a failed assertion also unblocks runtime shutdown.
                let _ = released.blocking_recv();
            });
            running.await.unwrap();
            let Response::MiningCandidate(Some(mut guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap()
            else {
                panic!("the first candidate reserves the computation slot");
            };
            let permit = Arc::downgrade(guard.0.borrow().as_ref().unwrap());
            while permit.strong_count() < 2 {
                tokio::task::yield_now().await;
            }
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            guard
                .0
                .wait_for(Option::is_none)
                .await
                .expect("unpublished work releases its guard's reservation at its deadline");
            tokio::time::resume();
            assert_eq!(permit.strong_count(), 1);
            assert!(matches!(
                state
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::MiningCandidate(block.clone()))
                    .await
                    .unwrap(),
                Response::MiningCandidate(None)
            ));
            assert!(!changes.0.has_changed().unwrap());

            release.send(()).unwrap();
            blocker.await.unwrap();
            // This read drains the expired computation on the sole blocking thread.
            assert_eq!(
                read.oneshot(ReadRequest::MiningCandidate).await.unwrap(),
                ReadResponse::MiningCandidate(None)
            );
            assert_eq!(permit.strong_count(), 0);
            let Response::MiningCandidate(Some(next_guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap()
            else {
                panic!("completed expired work must not reserve the next candidate's slot");
            };
            changes.changed().await;
            assert!(changes.is_current(block.hash()));
            drop((guard, next_guard));
        })
        .await
        .expect("candidate timeout must bound queued work without retaining failed admission");
    });
}
