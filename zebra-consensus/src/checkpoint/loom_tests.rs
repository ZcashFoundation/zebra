//! Loom models for retained-block admission, [`RetainedBlockPermit::try_admit`].
//!
//! The checkpoint verifier admits blocks from one task, through `&mut self`, while commit
//! tasks drop their permits on other threads. The models check that:
//!
//! - the count equals the number of live permits once every thread has finished, so a drop
//!   never takes it below zero. Atomic `fetch_sub` wraps even with overflow checks, so the
//!   models assert the count itself;
//! - concurrent drops only make the admission check conservative, so the single admitter
//!   never takes the count above the limit;
//! - an eviction frees its slot on the admitter's thread before the `fetch_add`;
//! - `Relaxed` is enough, because the counter publishes no other data.
//!
//! The negative control shows that the `&mut self` serialization is load-bearing: two
//! concurrent admitters overshoot the limit.
//!
//! Both positive models are exhaustive. Measured with `LOOM_LOG=loom::model=info`:
//! `count_matches_live_permits_and_never_exceeds_limit` completes in 1,706 iterations and
//! `eviction_frees_a_slot_before_admission` in 35. See `book/src/dev/loom-testing.md`.

use std::sync::Arc;

use ::loom::thread;

use crate::loom::sync::atomic::{AtomicUsize, Ordering};

use super::RetainedBlockPermit;

/// The smallest limit that lets two permits be live at once.
const LIMIT: usize = 2;

fn count(counter: &AtomicUsize) -> usize {
    counter.load(Ordering::Relaxed)
}

/// Admits a block without evicting, as `queue_block` does when nothing can be evicted.
fn admit(counter: &Arc<AtomicUsize>) -> Option<RetainedBlockPermit> {
    RetainedBlockPermit::try_admit(counter.clone(), LIMIT, || false)
}

/// Two permits are dropped on other threads, as commit tasks do, while the admitter keeps
/// admitting without evicting.
#[test]
fn count_matches_live_permits_and_never_exceeds_limit() {
    ::loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let first = admit(&counter).expect("an empty count is below the limit");
        let second = admit(&counter).expect("one live permit is below the limit");
        assert!(admit(&counter).is_none(), "admission stops at the limit");

        let droppers = [first, second].map(|permit| thread::spawn(move || drop(permit)));
        let mut live = Vec::new();
        for _ in 0..LIMIT {
            if let Some(permit) = admit(&counter) {
                assert!(
                    count(&counter) <= LIMIT,
                    "admission never exceeds the limit"
                );
                live.push(permit);
            }
        }
        for dropper in droppers {
            dropper
                .join()
                .expect("a panicking model thread fails the model before the join");
        }

        assert_eq!(
            count(&counter),
            live.len(),
            "the count equals the live permits"
        );
    });
}

/// At the limit, the admitter's eviction drops one of its own permits, as `evict_for` drops
/// the rejected `QueuedBlock` before it returns `true`, while a commit task drops the other
/// permit on another thread. Either release frees the slot the admission needs.
#[test]
fn eviction_frees_a_slot_before_admission() {
    ::loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut queued = admit(&counter);
        let committing = admit(&counter).expect("one live permit is below the limit");
        let dropper = thread::spawn(move || drop(committing));

        let admitted =
            RetainedBlockPermit::try_admit(counter.clone(), LIMIT, || match queued.take() {
                Some(evicted) => {
                    drop(evicted);
                    true
                }
                None => false,
            })
            .expect("the eviction or the concurrent drop frees a slot");
        assert!(
            count(&counter) <= LIMIT,
            "admission never exceeds the limit"
        );

        dropper
            .join()
            .expect("a panicking model thread fails the model before the join");
        let live = usize::from(queued.is_some()) + 1;
        assert_eq!(count(&counter), live, "the count equals the live permits");
        drop(admitted);
    });
}

/// Negative control for the `&mut self` serialization that `try_admit` relies on: two
/// admitters on different threads can both pass the check and overshoot the limit.
#[test]
#[should_panic(expected = "admission never exceeds the limit")]
fn two_admitters_overshoot_the_limit() {
    ::loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let admitters = [counter.clone(), counter.clone()].map(|counter| {
            thread::spawn(move || RetainedBlockPermit::try_admit(counter, 1, || false))
        });
        let permits = admitters.map(|admitter| {
            admitter
                .join()
                .expect("a panicking model thread fails the model before the join")
        });

        assert!(count(&counter) <= 1, "admission never exceeds the limit");
        drop(permits);
    });
}
