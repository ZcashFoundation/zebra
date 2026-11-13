//! Loom models of the early-relay `context_version` seqlock.
//!
//! The writer is [`EarlyRelayState::updating_context`] followed by the drop of its guard, as
//! the block write tasks hold it. The reader is [`EarlyRelayState::context_snapshot`], the
//! state reads, then [`EarlyRelayState::context_unchanged`], as in
//! `ReadStateService::early_relay`. All three run on the real struct.
//!
//! The state the reader reads is two cells, one per read. The writer stores generation `1`
//! in both between its two increments, and the main thread checks that every snapshot a
//! reader admitted carries one generation. The cells are loom atomics, not a loom
//! `UnsafeCell`: an `UnsafeCell` fails the model on the unsynchronized overlap that the
//! seqlock tolerates and then rejects, while RocksDB reads are free of data races. That they
//! also pair release/acquire across the writer and reader threads is the assumed, unverified
//! property that the `Release`/`Acquire` cells below stand for.
//!
//! # What the models show
//!
//! - With cells stored `Release` and loaded `Acquire`, which is how production assumes
//!   RocksDB behaves (see the `# Correctness` comment in `ReadStateService::early_relay`),
//!   every admitted snapshot is consistent, and the version is even at quiescence.
//! - With `Relaxed` cells, a store that does not synchronize, a reader admits a torn
//!   snapshot despite the re-check: the `SeqCst` version loads do not order the reads before
//!   the re-check. This is why production relies on RocksDB's internal synchronization.
//! - A `Release` fence right after the writer's first increment and an `Acquire` fence right
//!   before the re-check make `Relaxed` cells consistent, so the protocol would no longer
//!   rely on RocksDB. Either fence alone is not enough.
//! - Without the re-check, a reader admits a torn snapshot even from synchronizing cells.
//!
//! # Sizes
//!
//! Measured with `LOOM_LOG=loom::model=info` on a release build with loom 0.7.2. Threads
//! include the main thread. Each negative control stops at its first violation, the torn
//! snapshot `[0, 1]`, in about 0.02 seconds.
//!
//! | Model | Threads | Iterations |
//! | ----- | ------- | ---------- |
//! | `rechecked_reads_are_consistent_with_one_reader` | 3 | 1,053, exhaustive |
//! | `rechecked_reads_are_consistent_with_two_readers` | 4 | 20,528, preemption bound 3 |
//! | `fenced_relaxed_reads_are_consistent` | 3 | 1,053, exhaustive |
//! | `relaxed_cells_are_torn_despite_the_recheck` | 3 | torn snapshot admitted |
//! | `acquire_fence_without_release_fence_is_torn` | 3 | torn snapshot admitted |
//! | `release_fence_without_acquire_fence_is_torn` | 3 | torn snapshot admitted |
//! | `missing_recheck_is_caught` | 3 | torn snapshot admitted |
//!
//! The two-reader model does not complete exhaustively: it ran past 1,580,000 iterations in
//! 60 seconds without finding a violation, so it bounds itself in code. `Builder::check`
//! installs no `LOOM_LOG` subscriber, so its count was measured by running the same body
//! through `loom::model` with `LOOM_MAX_PREEMPTIONS=3`, which explores the same executions.

use std::sync::Arc;

use ::loom::{
    model::Builder,
    sync::atomic::{fence, AtomicU64, Ordering},
    thread,
};

use super::EarlyRelayState;

/// How the writer's mutation and the reader's reads synchronize, and which checks run.
#[derive(Clone, Copy, Debug)]
struct Variant {
    /// The ordering of the writer's cell stores.
    store: Ordering,
    /// The ordering of the reader's cell loads.
    load: Ordering,
    /// A `fence(Release)` right after `updating_context()` returns.
    writer_fence: bool,
    /// A `fence(Acquire)` after the cell loads, right before `context_unchanged()`.
    reader_fence: bool,
    /// Whether the reader calls `context_unchanged()` at all.
    recheck: bool,
}

/// Production as written, with RocksDB performing release/acquire operations internally.
const SYNCHRONIZING_DB: Variant = Variant {
    store: Ordering::Release,
    load: Ordering::Acquire,
    writer_fence: false,
    reader_fence: false,
    recheck: true,
};

/// Production as written, with a store that does not synchronize.
const UNSYNCHRONIZED_DB: Variant = Variant {
    store: Ordering::Relaxed,
    load: Ordering::Relaxed,
    ..SYNCHRONIZING_DB
};

/// Explicit fences, so the protocol no longer relies on the store's synchronization.
const FENCED: Variant = Variant {
    writer_fence: true,
    reader_fence: true,
    ..UNSYNCHRONIZED_DB
};

/// Runs one writer and `readers` readers, then checks on the main thread that every admitted
/// snapshot carries one generation and that the version counts both writer increments.
fn check_admitted_snapshots(variant: Variant, readers: usize) {
    let state = EarlyRelayState::new(None);
    let cells = Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);

    let writer = {
        let state = state.clone();
        let cells = cells.clone();
        thread::spawn(move || {
            let update = state.updating_context();
            if variant.writer_fence {
                fence(Ordering::Release);
            }
            for cell in cells.iter() {
                cell.store(1, variant.store);
            }
            drop(update);
        })
    };

    let readers: Vec<_> = (0..readers)
        .map(|_| {
            let state = state.clone();
            let cells = cells.clone();
            thread::spawn(move || {
                let version = state.context_snapshot()?;
                let snapshot = [cells[0].load(variant.load), cells[1].load(variant.load)];
                if variant.reader_fence {
                    fence(Ordering::Acquire);
                }
                if variant.recheck && !state.context_unchanged(version) {
                    return None;
                }
                Some(snapshot)
            })
        })
        .collect();

    writer.join().expect("the writer thread does not panic");
    for reader in readers {
        if let Some([first, second]) = reader.join().expect("reader threads do not panic") {
            assert_eq!(first, second, "an admitted snapshot carries one generation");
        }
    }

    assert_eq!(
        state.context_snapshot(),
        Some(2),
        "the version is even at quiescence and counts both writer increments",
    );
}

#[test]
fn rechecked_reads_are_consistent_with_one_reader() {
    ::loom::model(|| check_admitted_snapshots(SYNCHRONIZING_DB, 1));
}

/// Unbounded, this model ran past 1.58 million iterations in 60 seconds without completing.
#[test]
fn rechecked_reads_are_consistent_with_two_readers() {
    let mut builder = Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| check_admitted_snapshots(SYNCHRONIZING_DB, 2));
}

#[test]
fn fenced_relaxed_reads_are_consistent() {
    ::loom::model(|| check_admitted_snapshots(FENCED, 1));
}

#[test]
#[should_panic(expected = "an admitted snapshot carries one generation")]
fn relaxed_cells_are_torn_despite_the_recheck() {
    ::loom::model(|| check_admitted_snapshots(UNSYNCHRONIZED_DB, 1));
}

#[test]
#[should_panic(expected = "an admitted snapshot carries one generation")]
fn acquire_fence_without_release_fence_is_torn() {
    ::loom::model(|| {
        check_admitted_snapshots(
            Variant {
                writer_fence: false,
                ..FENCED
            },
            1,
        )
    });
}

#[test]
#[should_panic(expected = "an admitted snapshot carries one generation")]
fn release_fence_without_acquire_fence_is_torn() {
    ::loom::model(|| {
        check_admitted_snapshots(
            Variant {
                reader_fence: false,
                ..FENCED
            },
            1,
        )
    });
}

#[test]
#[should_panic(expected = "an admitted snapshot carries one generation")]
fn missing_recheck_is_caught() {
    ::loom::model(|| {
        check_admitted_snapshots(
            Variant {
                recheck: false,
                ..SYNCHRONIZING_DB
            },
            1,
        )
    });
}
