//! Loom model for the block crypto drop-flush, [`Queueing`].
//!
//! A block registers one slot per shielded transaction. Each transaction's guard attaches its
//! request guards before it releases its slot, and the last guard to drop flushes every
//! verifier its requests were queued in. With two transactions on two threads, the model
//! checks that:
//!
//! - each queued verifier is flushed exactly once, after the block's last guard is dropped,
//!   and the flushes carry both transactions' verifier bits;
//! - guards dropped on different threads never double-flush or lose a bit;
//! - `pending` never underflows: the loom build has overflow checks, so a double decrement
//!   panics with "attempt to subtract with overflow".
//!
//! The model calls `Queueing::new` with the transaction's progress directly, as
//! `transaction.scope(Queueing::for_request)` does. tokio's task-local `CURRENT` is a
//! thread-local slot that every loom thread shares, so a scope spanning a loom switch would
//! leak into another model thread. The flush goes through the `tests::FLUSH` hook, installed
//! around the whole model: every model thread runs on the test's OS thread, so the hook is
//! visible to all of them. Its recorders are `std` atomics, because the hook outlives each
//! execution.
//!
//! The hook only sees the verifiers a flush calls, so the model checks neither of these: a
//! flush that finds no queued bits calls no hook, and the hook cannot tell whether the
//! flushing thread still holds the progress lock.
//!
//! The negative control releases each transaction's slot before attaching its request
//! guard, which lets the block flush before the other transaction's bit is queued.
//!
//! `two_transactions_flush_once` is exhaustive: 1,134 iterations, measured with
//! `LOOM_LOG=loom::model=info`. Keep it at two transactions: when the model was sized, a
//! third did not finish exhaustively in seven minutes. See `book/src/dev/loom-testing.md`.

use std::sync::{
    atomic::{AtomicU8, AtomicUsize, Ordering},
    Arc,
};

use ::loom::thread;

use crate::BoxError;

use super::{tests::FLUSH, BlockFlush, Queueing, Verifier};

/// The verifiers the two transactions queue their requests in, one each.
const VERIFIERS: [Verifier; 2] = [Verifier::Ed25519, Verifier::Sapling];

/// The flushes one execution made.
struct Flushes {
    /// Hook calls: one per queued verifier in each flush.
    calls: AtomicUsize,
    /// The union of the flushed verifier bits.
    verifiers: AtomicU8,
}

impl Flushes {
    const fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            verifiers: AtomicU8::new(0),
        }
    }
}

fn bit(verifier: Verifier) -> u8 {
    // There are only five verifier kinds, so every discriminant fits the bit mask.
    1 << verifier as u8
}

/// What a block transaction does: attach a request guard, queue it, then release the slot.
fn attach_then_release(context: &Arc<u64>, verifier: Verifier) {
    let transaction = Queueing::for_transaction(context)
        .expect("the main thread registers the block before it spawns the transactions");
    let request = Queueing::new(transaction.0.clone());
    request.enqueued(verifier);
    drop(transaction);
}

/// The ordering bug: release the slot before the request guard exists.
fn release_then_attach(context: &Arc<u64>, verifier: Verifier) {
    let transaction = Queueing::for_transaction(context)
        .expect("the main thread registers the block before it spawns the transactions");
    let progress = transaction.0.clone();
    drop(transaction);
    Queueing::new(progress).enqueued(verifier);
}

/// Registers one block with a slot per verifier, runs `transaction` for each verifier on its
/// own thread, and checks the flushes after the block is dropped.
fn drop_flush(flushes: &'static Flushes, transaction: fn(&Arc<u64>, Verifier)) {
    let hook = Box::new(move |verifier: Verifier| -> Result<bool, BoxError> {
        flushes.calls.fetch_add(1, Ordering::Relaxed);
        flushes.verifiers.fetch_or(bit(verifier), Ordering::Relaxed);
        Ok(true)
    });

    FLUSH.sync_scope(hook, || {
        ::loom::model(move || {
            flushes.calls.store(0, Ordering::Relaxed);
            flushes.verifiers.store(0, Ordering::Relaxed);

            let context = Arc::new(0_u64);
            let block = BlockFlush::register(&context, VERIFIERS.len());
            let transactions = VERIFIERS.map(|verifier| {
                let context = context.clone();
                thread::spawn(move || transaction(&context, verifier))
            });
            for transaction in transactions {
                transaction
                    .join()
                    .expect("a panicking model thread fails the model before the join");
            }
            drop(block);

            let all = VERIFIERS
                .into_iter()
                .fold(0, |bits, verifier| bits | bit(verifier));
            assert_eq!(
                (
                    flushes.calls.load(Ordering::Relaxed),
                    flushes.verifiers.load(Ordering::Relaxed),
                ),
                (VERIFIERS.len(), all),
                "each queued verifier is flushed exactly once, after the last guard is dropped"
            );
        });
    });
}

#[test]
fn two_transactions_flush_once() {
    static FLUSHES: Flushes = Flushes::new();
    drop_flush(&FLUSHES, attach_then_release);
}

/// Negative control: a transaction that releases its slot first lets the block flush early,
/// then flush again when the late request guards drop.
#[test]
#[should_panic(expected = "each queued verifier is flushed exactly once")]
fn releasing_the_slot_first_flushes_early() {
    static FLUSHES: Flushes = Flushes::new();
    drop_flush(&FLUSHES, release_then_attach);
}
