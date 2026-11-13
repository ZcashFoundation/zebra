//! Cancellation tests for queued CPU verification.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use futures::FutureExt;

/// Dropped verification must not spend CPU when its queued job eventually reaches a worker.
#[test]
fn cancelled_queued_work_is_skipped() {
    let _init_guard = zebra_test::init();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let executed = ran.clone();
    let (drained, finished) = mpsc::channel();
    pool.install(|| {
        // The only worker is occupied until both jobs are queued and the response is dropped.
        assert!(
            super::spawn_fifo(move || executed.store(true, Ordering::SeqCst))
                .now_or_never()
                .is_none()
        );
        rayon::spawn_fifo(move || drained.send(()).unwrap());
    });
    finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the worker drains the cancelled verification job");
    assert!(!ran.load(Ordering::SeqCst));
}
