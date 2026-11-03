#![allow(clippy::unwrap_in_result)]

use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

mod commit_phases;
mod prop;
mod tree_first;
mod tree_first_gate;
mod vectors;

thread_local! {
    static WAIT_FOR_EARLY_CHECK_FAILURE: Cell<bool> = const { Cell::new(false) };
}

static EARLY_CHECK_FAILED_FOR_TEST: AtomicBool = AtomicBool::new(false);

fn arm_early_check_failure_gate() {
    EARLY_CHECK_FAILED_FOR_TEST.store(false, Ordering::Relaxed);
    WAIT_FOR_EARLY_CHECK_FAILURE.with(|gate| gate.set(true));
}

fn disarm_early_check_failure_gate() {
    WAIT_FOR_EARLY_CHECK_FAILURE.with(|gate| gate.set(false));
}

pub(super) fn record_early_check_failure() {
    EARLY_CHECK_FAILED_FOR_TEST.store(true, Ordering::Release);
}

pub(super) fn wait_for_early_check_failure_before_tree_update() {
    if !WAIT_FOR_EARLY_CHECK_FAILURE.with(Cell::get) {
        return;
    }

    let timeout = Instant::now() + Duration::from_secs(10);
    while !EARLY_CHECK_FAILED_FOR_TEST.load(Ordering::Acquire) {
        assert!(
            Instant::now() < timeout,
            "the contextual check did not reject the block"
        );
        std::thread::yield_now();
    }
}
