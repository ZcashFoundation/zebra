//! Block batch-flush registration regressions.

use std::sync::{Arc, Barrier};

use super::{
    block_verifier_batch_flush_key, block_verifier_batch_flush_ready,
    register_block_verifier_batch_flush,
};

#[test]
fn boundary_waits_for_every_transaction_and_fires_once() {
    let context = Arc::new(());
    let key = block_verifier_batch_flush_key(&context);
    let _guard = register_block_verifier_batch_flush(&context, 3);
    assert!(!block_verifier_batch_flush_ready(key));
    assert!(!block_verifier_batch_flush_ready(key));
    assert!(block_verifier_batch_flush_ready(key));
    assert!(!block_verifier_batch_flush_ready(key));
}

#[test]
fn cancelled_registration_does_not_flush() {
    let context = Arc::new(());
    let key = block_verifier_batch_flush_key(&context);
    let guard = register_block_verifier_batch_flush(&context, 2);
    assert!(!block_verifier_batch_flush_ready(key));
    drop(guard);
    assert!(!block_verifier_batch_flush_ready(key));
}

#[test]
fn concurrent_blocks_and_transactions_keep_independent_flushes() {
    let first = Arc::new(());
    let second = Arc::new(());
    let first_key = block_verifier_batch_flush_key(&first);
    let second_key = block_verifier_batch_flush_key(&second);
    let _first_guard = register_block_verifier_batch_flush(&first, 4);
    let _second_guard = register_block_verifier_batch_flush(&second, 4);
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        let mut threads = Vec::new();
        for key in [first_key, second_key].into_iter().cycle().take(8) {
            let barrier = barrier.clone();
            threads.push(scope.spawn(move || {
                barrier.wait();
                (key, block_verifier_batch_flush_ready(key))
            }));
        }
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        for key in [first_key, second_key] {
            assert_eq!(
                results
                    .iter()
                    .filter(|(candidate, flush)| *candidate == key && *flush)
                    .count(),
                1
            );
            assert!(!block_verifier_batch_flush_ready(key));
        }
    });
}

#[test]
fn empty_block_registration_never_flushes() {
    let context = Arc::new(());
    let key = block_verifier_batch_flush_key(&context);
    let _guard = register_block_verifier_batch_flush(&context, 0);
    assert!(!block_verifier_batch_flush_ready(key));
}

#[test]
fn concurrent_registrations_do_not_replace_other_blocks() {
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let context = Arc::new(());
                    let key = block_verifier_batch_flush_key(&context);
                    barrier.wait();
                    let _guard = register_block_verifier_batch_flush(&context, 2);
                    barrier.wait();
                    assert!(!block_verifier_batch_flush_ready(key));
                    assert!(block_verifier_batch_flush_ready(key));
                    assert!(!block_verifier_batch_flush_ready(key));
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    });
}
