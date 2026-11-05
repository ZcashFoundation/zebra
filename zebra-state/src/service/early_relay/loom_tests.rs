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
//! seqlock tolerates and then rejects, while RocksDB reads are free of data races. Positive
//! models use `Relaxed` cells, so the production fences must provide all synchronization.
//!
//! # What the models show
//!
//! - The production `Release` and `Acquire` fences make every admitted relaxed-cell snapshot
//!   consistent, and the version is even at quiescence. Removing either fence independently
//!   makes the one-reader model admit the torn snapshot `[0, 1]`.
//! - Without the re-check, a reader admits a torn snapshot even from cells stored `Release`
//!   and loaded `Acquire`.
//!
//! # Sizes
//!
//! Measured with `LOOM_LOG=loom::model=info` on a release build with loom 0.7.2. Threads
//! include the main thread. The negative control stops at its first violation, the torn
//! snapshot `[0, 1]`, in about 0.02 seconds.
//!
//! | Model | Threads | Iterations |
//! | ----- | ------- | ---------- |
//! | `relaxed_cells_are_consistent_with_one_reader` | 3 | 1,053, exhaustive |
//! | `relaxed_cells_are_consistent_with_two_readers` | 4 | 20,528, preemption bound 3 |
//! | `missing_recheck_is_caught` | 3 | torn snapshot admitted |
//!
//! The two-reader model bounds itself in code to keep exploration tractable. `Builder::check`
//! installs no `LOOM_LOG` subscriber, so its count was measured by running the same body
//! through `loom::model` with `LOOM_MAX_PREEMPTIONS=3`, which explores the same executions.

use std::sync::Arc;

use ::loom::{
    model::Builder,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use super::EarlyRelayState;

/// How the writer's mutation and the reader's reads synchronize, and whether the reader rechecks.
#[derive(Clone, Copy, Debug)]
struct Variant {
    /// The ordering of the writer's cell stores.
    store: Ordering,
    /// The ordering of the reader's cell loads.
    load: Ordering,
    /// Whether the reader calls `context_unchanged()` at all.
    recheck: bool,
}

/// Cells that synchronize each store with its loads, isolating the missing-recheck control.
const SYNCHRONIZING_CELLS: Variant = Variant {
    store: Ordering::Release,
    load: Ordering::Acquire,
    recheck: true,
};

/// Cells whose atomic accesses provide no synchronization.
const RELAXED_CELLS: Variant = Variant {
    store: Ordering::Relaxed,
    load: Ordering::Relaxed,
    ..SYNCHRONIZING_CELLS
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
fn relaxed_cells_are_consistent_with_one_reader() {
    ::loom::model(|| check_admitted_snapshots(RELAXED_CELLS, 1));
}

#[test]
fn relaxed_cells_are_consistent_with_two_readers() {
    let mut builder = Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| check_admitted_snapshots(RELAXED_CELLS, 2));
}

#[test]
#[should_panic(expected = "an admitted snapshot carries one generation")]
fn missing_recheck_is_caught() {
    ::loom::model(|| {
        check_admitted_snapshots(
            Variant {
                recheck: false,
                ..SYNCHRONIZING_CELLS
            },
            1,
        )
    });
}
