//! Sends selected counters and gauges to Sentry once a minute.

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{atomic::Ordering, Arc, LazyLock, Mutex, PoisonError},
    time::Duration,
};

use metrics::{atomics::AtomicU64, Counter, Gauge, Key};
use sentry::protocol::Unit;

/// How often the listed counters and gauges are sent to Sentry.
///
/// Each label set sends one value per interval, so the volume Sentry stores is inversely
/// proportional to this.
pub(super) const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// The counters sent to Sentry, as the total recorded since the previous [`flush`].
///
/// Sentry stores every value it receives, and these can be incremented many times a second, so
/// each label set is sent as one total per [`FLUSH_INTERVAL`].
const COUNTERS: &[&str] = &[
    "sync.restart.count",
    "sync.block.error.count",
    "sync.verified.block.count",
    "gossip.verified.block.count",
];

/// The gauges sent to Sentry, as their latest value at each [`flush`], with their units.
const GAUGES: &[(&str, Option<Unit>)] = &[
    ("sync.downloads.in_flight", None),
    ("sync.downloads.oldest_age_seconds", Some(Unit::Second)),
    ("zcash.mempool.size.transactions", None),
    (
        "zebra.state.rocksdb.total_disk_size_bytes",
        Some(Unit::Byte),
    ),
];

/// The listed counters and gauges recorded so far, by name and labels.
static RECORDED: LazyLock<Mutex<HashMap<Key, Periodic>>> = LazyLock::new(Default::default);

/// A listed counter's total since the last [`flush`], or a listed gauge's latest value as `f64`
/// bits.
#[derive(Clone)]
struct Periodic {
    kind: Kind,
    value: Arc<AtomicU64>,
}

#[derive(Clone)]
enum Kind {
    Counter,
    Gauge(Option<Unit>),
}

/// Returns a counter that [`flush`] sends to Sentry, if `key` is one of the [`COUNTERS`].
pub(super) fn counter(key: &Key) -> Option<Counter> {
    COUNTERS
        .contains(&key.name())
        .then(|| Counter::from_arc(recorded(key, Kind::Counter)))
}

/// Returns a gauge that [`flush`] sends to Sentry, if `key` is one of the [`GAUGES`].
pub(super) fn gauge(key: &Key) -> Option<Gauge> {
    let (_, unit) = GAUGES.iter().find(|(name, _)| *name == key.name())?;

    Some(Gauge::from_arc(recorded(key, Kind::Gauge(unit.clone()))))
}

fn recorded(key: &Key, kind: Kind) -> Arc<AtomicU64> {
    let mut recorded = RECORDED.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(periodic) = recorded.get(key) {
        return periodic.value.clone();
    }

    // A gauge is NaN until it is first set, so `flush` skips it.
    let initial = match kind {
        Kind::Counter => 0,
        Kind::Gauge(_) => f64::NAN.to_bits(),
    };
    let value = Arc::new(AtomicU64::new(initial));
    recorded.insert(
        key.clone(),
        Periodic {
            kind,
            value: value.clone(),
        },
    );
    value
}

/// Held for a whole [`flush`], so that the flush at shutdown waits for a periodic flush that has
/// taken counter totals but not sent them yet.
static FLUSHING: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether this thread is in a [`flush`], so that the panic hook's flush returns instead of
    /// waiting for a flush that panicked on the same thread.
    static IN_FLUSH: Cell<bool> = const { Cell::new(false) };
}

/// Sends each listed counter's total since the previous call, and each listed gauge's latest
/// value.
///
/// Sends after releasing the lock on the recorded metrics, so it can also run from the panic hook.
pub(super) fn flush() {
    if IN_FLUSH.get() {
        return;
    }

    let _flushing = FLUSHING.lock().unwrap_or_else(PoisonError::into_inner);
    IN_FLUSH.set(true);
    send_recorded();
    IN_FLUSH.set(false);
}

fn send_recorded() {
    let recorded: Vec<_> = RECORDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(key, periodic)| (key.clone(), periodic.clone()))
        .collect();

    for (key, periodic) in recorded {
        let name = key.name().to_owned();
        let labels = key
            .labels()
            .map(|label| (label.key().to_owned(), label.value().to_owned()));

        match periodic.kind {
            Kind::Counter => {
                let count = periodic.value.swap(0, Ordering::Relaxed);
                if count == 0 {
                    continue;
                }

                // One minute's count is far below f64's exact integer range.
                let mut counter = sentry::metrics::counter(name, count as f64);
                for (key, value) in labels {
                    counter = counter.attribute(key, value);
                }
                counter.capture();
            }
            Kind::Gauge(unit) => {
                let value = f64::from_bits(periodic.value.load(Ordering::Relaxed));
                if value.is_nan() {
                    continue;
                }

                let mut gauge = sentry::metrics::gauge(name, value);
                if let Some(unit) = unit {
                    gauge = gauge.unit(unit);
                }
                for (key, value) in labels {
                    gauge = gauge.attribute(key, value);
                }
                gauge.capture();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn flush_waits_for_a_flush_in_progress() {
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        // Stands in for a periodic flush that has taken the totals but not sent them yet.
        let in_progress = thread::spawn(move || {
            let _flushing = FLUSHING.lock().unwrap_or_else(PoisonError::into_inner);
            locked_tx.send(()).expect("the test thread is waiting");
            release_rx
                .recv()
                .expect("the test thread releases the flush");
        });
        locked_rx
            .recv()
            .expect("the other thread holds the flush lock");

        let (flushed_tx, flushed_rx) = mpsc::channel();
        let flushing = thread::spawn(move || {
            flush();
            flushed_tx.send(()).expect("the test thread is waiting");
        });

        assert!(
            flushed_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "a flush finished while another one was in progress"
        );
        release_tx.send(()).expect("the other thread is waiting");
        flushed_rx
            .recv()
            .expect("the flush finishes once the other one does");

        in_progress.join().expect("the other thread doesn't panic");
        flushing.join().expect("the flushing thread doesn't panic");
    }

    #[test]
    fn flush_returns_when_reentered_from_a_flush_on_the_same_thread() {
        // A panic during a flush runs the panic hook's flush on the same thread.
        let _flushing = FLUSHING.lock().unwrap_or_else(PoisonError::into_inner);
        IN_FLUSH.set(true);

        flush();

        IN_FLUSH.set(false);
    }
}
