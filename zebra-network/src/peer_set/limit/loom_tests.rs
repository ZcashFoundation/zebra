//! Loom models for [`ActiveConnectionCounter`] and [`ConnectionTracker`](super::ConnectionTracker).
//!
//! The counter owns the receiving end of one channel and every tracker holds a sender. A
//! tracker reserves a slot when it is created, sends `Opened` once (from `mark_open`, or from
//! `Drop` if it was never marked open), and sends `Closed` from `Drop`. The owner drains the
//! channel in `update_count`, which moves a slot from `reserved_count` to `count` on `Opened`
//! and releases it on `Closed`.
//!
//! Both models drop two trackers on two threads, one marked open twice and one never, while
//! the owner drains on the main thread. [`two_trackers_release_their_slots_exactly_once`]
//! checks that:
//!
//! - a drain releases the slot of every tracker whose drop has returned, the number of slots a
//!   drain reports never grows, and it is zero once both trackers are dropped;
//! - neither `reserved_count -= 1` nor `count -= 1` underflows: these builds have overflow
//!   checks, so an underflow panics instead of wrapping;
//! - `mark_open` is idempotent, and a tracker dropped before `mark_open` still releases its
//!   reservation exactly once (the class of bug fixed in #10311).
//!
//! [`two_trackers_hold_their_slots_until_dropped`] checks the same and adds the lower bound: a
//! drain keeps the slot of every tracker whose drop has not started. The two bounds are the
//! equality "`count + reserved_count` equals the number of live trackers", as far as a drain
//! can observe it while drops are in flight.
//!
//! The protocol depends on one ordering: a tracker's `Opened` enters the channel before its
//! `Closed`, because one sender sends both in the tracker's own program order.
//! [`open_sent_apart_from_its_tracker_underflows_the_count`] is the negative control: it sends
//! `Opened` from another thread, and loom finds the schedule where the owner drains `Closed`
//! first. That per-sender order is the only ordering these models assert. The `dropping` and
//! `dropped` observers are test scaffolding and make no memory-ordering claim: loom's
//! `try_recv` reads the channel's message count from global execution state, not from a
//! modelled atomic, so a drain would receive a `Closed` even with `Relaxed` observers.
//!
//! # Why the models are bounded
//!
//! Loom's unbounded search does not explore every order of a drain against a send. Its
//! partial-order reduction treats a send and a receive on the same channel as independent
//! (`loom-0.7.2/src/rt/mpsc.rs:165-170`), and a `try_recv` on an empty channel records no
//! access (`src/sync/mpsc.rs:73-79`), so it reaches only the drain positions that the
//! observers' atomics happen to induce. A preemption-bounded search adds conservative
//! backtracking points (`src/rt/path.rs:362-385`) that reach drain positions the unbounded
//! search misses. With `update_count` returning `count.max(reserved_count)` instead of the sum,
//! [`two_trackers_release_their_slots_exactly_once`] passes when unbounded, and fails "the
//! drained count never grows" at every bound from 2 to 8. So both models set
//! `preemption_bound` in code, at the largest bound under the design's budget of about 10⁵
//! iterations (§2.7), and neither is exhaustive.
//!
//! Iterations, measured under `RUSTFLAGS="--cfg zebra_loom -C debug-assertions"` in a release
//! build with a temporary counter in the model closure (`Builder::check` logs nothing under
//! `LOOM_LOG`):
//!
//! - [`two_trackers_release_their_slots_exactly_once`], bound 5: 47,309 (1.4 s); bound 4 is
//!   14,231 and bound 6 is 122,339;
//! - [`two_trackers_hold_their_slots_until_dropped`], bound 4: 61,282 (1.7 s); bound 3 is 9,526,
//!   bound 5 is 300,884, and unbounded it takes 2,113,702 (56 s);
//! - [`open_sent_apart_from_its_tracker_underflows_the_count`] stays unbounded, because loom
//!   does reorder two sends on the same channel, and fails in under 0.05 s.

use std::sync::Arc;

use ::loom::{
    model::Builder,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use super::{ActiveConnectionCounter, ConnectionStatus};

/// The number of trackers, and connection slots, in the two-tracker models.
const TRACKERS: usize = 2;

/// Returns a model builder that explores schedules with at most `preemptions` preemptions.
fn bounded(preemptions: usize) -> Builder {
    let mut builder = Builder::new();
    builder.preemption_bound = Some(preemptions);
    builder
}

/// Two trackers on two threads, one marked open twice and one never, while the owner drains.
#[test]
fn two_trackers_release_their_slots_exactly_once() {
    bounded(5).check(|| {
        let mut counter = ActiveConnectionCounter::new_counter();
        let mut opened_twice = counter.track_connection();
        let never_opened = counter.track_connection();
        assert_eq!(
            counter.update_count(),
            TRACKERS,
            "each tracker reserves one slot when it is created",
        );

        // Observer: each tracker thread counts itself once its tracker's drop has returned.
        let dropped = Arc::new(AtomicUsize::new(0));

        let opener = {
            let dropped = dropped.clone();
            thread::spawn(move || {
                opened_twice.mark_open();
                opened_twice.mark_open();
                drop(opened_twice);
                dropped.fetch_add(1, Ordering::Release);
            })
        };
        let closer = {
            let dropped = dropped.clone();
            thread::spawn(move || {
                drop(never_opened);
                dropped.fetch_add(1, Ordering::Release);
            })
        };

        let mut previous = TRACKERS;
        loop {
            // A tracker counted here has sent its `Closed`, so the drain below receives it.
            let dropped_before = dropped.load(Ordering::Acquire);
            let live = counter.update_count();

            assert!(
                live <= TRACKERS - dropped_before,
                "a drain releases the slot of every tracker that has been dropped",
            );
            assert!(
                live <= previous,
                "the drained count never grows: no slot is added back",
            );
            previous = live;

            if dropped_before == TRACKERS {
                break;
            }
            thread::yield_now();
        }

        opener
            .join()
            .expect("a panic on a loom thread fails the model before join returns");
        closer
            .join()
            .expect("a panic on a loom thread fails the model before join returns");

        assert_eq!(
            (
                counter.update_count(),
                counter.count,
                counter.reserved_count
            ),
            (0, 0, 0),
            "both slots are released exactly once at quiescence",
        );
    });
}

/// The same two trackers, with an observer on each side of each drop, so a drain also checks
/// that no slot is released before its tracker's drop starts.
#[test]
fn two_trackers_hold_their_slots_until_dropped() {
    bounded(4).check(|| {
        let mut counter = ActiveConnectionCounter::new_counter();
        let mut opened_twice = counter.track_connection();
        let never_opened = counter.track_connection();
        assert_eq!(
            counter.update_count(),
            TRACKERS,
            "each tracker reserves one slot when it is created",
        );

        // Observers: each tracker thread counts itself in `dropping` just before its tracker's
        // drop starts, and in `dropped` once that drop has returned.
        let dropping = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));

        let opener = {
            let dropping = dropping.clone();
            let dropped = dropped.clone();
            thread::spawn(move || {
                opened_twice.mark_open();
                opened_twice.mark_open();
                dropping.fetch_add(1, Ordering::Release);
                drop(opened_twice);
                dropped.fetch_add(1, Ordering::Release);
            })
        };
        let closer = {
            let dropping = dropping.clone();
            let dropped = dropped.clone();
            thread::spawn(move || {
                dropping.fetch_add(1, Ordering::Release);
                drop(never_opened);
                dropped.fetch_add(1, Ordering::Release);
            })
        };

        let mut previous = TRACKERS;
        loop {
            // A tracker counted in `dropped_before` has sent its `Closed`, so the drain receives
            // it; a tracker missing from `dropping_after` has not sent it yet.
            let dropped_before = dropped.load(Ordering::Acquire);
            let live = counter.update_count();
            let dropping_after = dropping.load(Ordering::Acquire);

            assert!(
                live <= TRACKERS - dropped_before,
                "a drain releases the slot of every tracker that has been dropped",
            );
            assert!(
                live >= TRACKERS - dropping_after,
                "a drain keeps the slot of every tracker whose drop has not started",
            );
            assert!(
                live <= previous,
                "the drained count never grows: no slot is added back",
            );
            previous = live;

            if dropped_before == TRACKERS {
                break;
            }
            thread::yield_now();
        }

        opener
            .join()
            .expect("a panic on a loom thread fails the model before join returns");
        closer
            .join()
            .expect("a panic on a loom thread fails the model before join returns");

        assert_eq!(
            (
                counter.update_count(),
                counter.count,
                counter.reserved_count
            ),
            (0, 0, 0),
            "both slots are released exactly once at quiescence",
        );
    });
}

/// Negative control: one tracker's `Opened` is sent from another thread, so nothing orders it
/// before the tracker's `Closed`.
#[test]
#[should_panic(expected = "attempt to subtract with overflow")]
fn open_sent_apart_from_its_tracker_underflows_the_count() {
    ::loom::model(|| {
        let mut counter = ActiveConnectionCounter::new_counter();
        let mut tracker = counter.track_connection();

        // Mark the tracker open without sending, so its drop sends only `Closed`, and send its
        // `Opened` on a separate sender instead.
        tracker.has_marked_open = true;
        let open_tx = counter.status_notification_tx.clone();

        let opener = thread::spawn(move || {
            let _ = open_tx.send(ConnectionStatus::Opened);
        });
        let closer = thread::spawn(move || drop(tracker));

        opener
            .join()
            .expect("a panic on a loom thread fails the model before join returns");
        closer
            .join()
            .expect("a panic on a loom thread fails the model before join returns");

        // On the schedule where `Closed` was sent first, this drain decrements a zero `count`.
        counter.update_count();
    });
}
