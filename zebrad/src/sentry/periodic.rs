//! Sends selected counters and gauges to Sentry once a minute.

use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, LazyLock, Mutex, PoisonError},
};

use metrics::{atomics::AtomicU64, Counter, Gauge, Key};
use sentry::protocol::Unit;

/// The counters sent to Sentry, as the total recorded since the previous [`flush`].
///
/// Sentry stores every value it receives, and these can be incremented many times a second, so
/// each label set is sent as one total a minute.
const COUNTERS: &[&str] = &["sync.restart.count", "sync.block.error.count"];

/// The gauges sent to Sentry, as their latest value at each [`flush`], with their units.
const GAUGES: &[(&str, Option<Unit>)] = &[
    ("sync.downloads.in_flight", None),
    ("sync.downloads.oldest_age_seconds", Some(Unit::Second)),
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

/// Sends each listed counter's total since the previous call, and each listed gauge's latest
/// value.
///
/// Sends after releasing the lock, so it can also run from the panic hook.
pub(crate) fn flush() {
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
