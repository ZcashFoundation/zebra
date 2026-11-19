//! The state answers requests for a non-finalized best tip, for its mining candidate, and for a
//! block template's chain info from memory, without a blocking thread.

use std::time::Duration;

use tower::{Service, ServiceExt};
use zebra_chain::serialization::DateTime32;

use crate::{
    service::{
        arbitrary::TEST_MINING_CANDIDATE_LIFETIME, check::difficulty::pow_adjustment_block_span,
        read,
    },
    CheckpointVerifiedBlock, ReadRequest, ReadResponse, Request, Response,
};

use super::mining_rejection::{candidate, state};

#[test]
fn in_memory_tip_and_mining_candidate_reads_need_no_blocking_thread() {
    let _init_guard = zebra_test::init();

    // The states are created on their own runtime: committing their blocks needs blocking
    // threads, and a multi-threaded runtime.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (non_finalized, finalized_only) = setup.block_on(async {
        let (_state, non_finalized) = state(false).await;
        let (_state, finalized_only) = state(true).await;
        (non_finalized, finalized_only)
    });
    let expected_tip = non_finalized.best_tip();
    assert!(expected_tip.is_some());

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, and queue a probe that shows it stays busy.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();
            let probe = tokio::task::spawn_blocking(|| ());

            let answer = |state: &crate::ReadStateService, request| {
                tokio::time::timeout(Duration::from_secs(5), state.clone().oneshot(request))
            };

            let tip = answer(&non_finalized, ReadRequest::Tip)
                .await
                .expect("a non-finalized tip is read from memory");
            assert!(matches!(tip, Ok(ReadResponse::Tip(tip)) if tip == expected_tip));

            // Without a published candidate, the answer doesn't depend on the tip, wherever
            // the tip is.
            for state in [&non_finalized, &finalized_only] {
                let candidate = answer(state, ReadRequest::MiningCandidate)
                    .await
                    .expect("the mining candidate is read from memory");
                assert!(matches!(candidate, Ok(ReadResponse::MiningCandidate(None))));
            }

            assert!(
                !probe.is_finished(),
                "the blocking thread was busy for all of these reads",
            );
            drop(release);
            busy.await.unwrap();
            probe.await.unwrap();
        });
    drop(setup);
}

/// A published candidate that extends a non-finalized tip must be read from memory too: the
/// template updater and a waiting miner's request read it on their way.
#[test]
fn published_mining_candidate_read_needs_no_blocking_thread() {
    let _init_guard = zebra_test::init();

    // The candidate is published on its own runtime, which keeps its publishing task running.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_state, read, block, _guard, _changes) = setup.block_on(async {
        let (mut state, read) = state(false).await;
        state.mining_candidate_lifetime = TEST_MINING_CANDIDATE_LIFETIME;
        let block = candidate(&read, false, 0).await;
        let ReadResponse::MiningCandidateChanges(mut changes) = read
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("the state returns a mining candidate subscription");
        };
        let Response::MiningCandidate(Some(guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("a child of the best tip is admitted");
        };
        changes.changed().await;
        (state, read, block, guard, changes)
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, and queue a probe that shows it stays busy.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();
            let probe = tokio::task::spawn_blocking(|| ());

            let published = tokio::time::timeout(
                Duration::from_secs(5),
                read.clone().oneshot(ReadRequest::MiningCandidate),
            )
            .await
            .expect("a published candidate on a non-finalized tip is read from memory");
            assert!(matches!(
                published,
                Ok(ReadResponse::MiningCandidate(Some(info))) if info.tip_hash == block.hash()
            ));

            assert!(
                !probe.is_finished(),
                "the blocking thread was busy for the read",
            );
            drop(release);
            busy.await.unwrap();
            probe.await.unwrap();
        });
    drop(setup);
}

/// The chain info for a block template must be computed without a blocking thread once the best
/// chain holds every header that the next block's difficulty adjustment reads, and it must
/// match what a read of the whole state returns. With one header fewer in the chain, the read
/// needs the finalized state, so it is left to a blocking thread.
#[test]
fn chain_info_read_needs_no_blocking_thread_once_the_chain_holds_its_headers() {
    let _init_guard = zebra_test::init();

    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    // Returns a state whose best chain has `missing` fewer blocks than the difficulty adjustment
    // of its next block reads.
    let chain_with_headers = |missing: usize| async move {
        let (mut state, read) = state(false).await;
        loop {
            let chain = read
                .latest_best_chain()
                .expect("the state has a non-finalized block");
            let next_height = chain.non_finalized_tip_height().next().unwrap();
            if chain.len() + missing >= pow_adjustment_block_span(&read.network, next_height) {
                return (state, read);
            }
            let block = candidate(&read, false, 0).await;
            state
                .ready()
                .await
                .unwrap()
                .call(Request::CommitSemanticallyVerifiedBlock(
                    CheckpointVerifiedBlock::from(block).into(),
                ))
                .await
                .unwrap();
        }
    };
    let ((_complete_state, complete), (_short_state, short)) =
        setup.block_on(async { (chain_with_headers(0).await, chain_with_headers(1).await) });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, so a read that needs one can't finish.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();

            // Both reads use the local clock, so they are repeated if the clock's second changes
            // between them.
            let (from_memory, from_state) = loop {
                let second = DateTime32::now();
                let from_memory = futures::poll!(complete.clone().oneshot(ReadRequest::ChainInfo));
                let from_state = read::difficulty::get_block_template_chain_info(
                    &complete.latest_non_finalized_state(),
                    &complete.db,
                    &complete.network,
                )
                .expect("the state has a tip");
                if DateTime32::now() == second {
                    break (from_memory, from_state);
                }
            };
            let std::task::Poll::Ready(Ok(ReadResponse::ChainInfo(from_memory))) = from_memory
            else {
                panic!("a chain with every header answers at once: {from_memory:?}");
            };
            assert_eq!(from_memory, from_state);

            let mut needs_database = Box::pin(short.clone().oneshot(ReadRequest::ChainInfo));
            assert!(
                futures::poll!(&mut needs_database).is_pending(),
                "a chain without every header leaves the read to a blocking thread",
            );

            drop(release);
            busy.await.unwrap();
            assert!(matches!(
                needs_database.await,
                Ok(ReadResponse::ChainInfo(_))
            ));
        });
    drop(setup);
}
