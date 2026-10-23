//! Sends selected counters and gauges to Sentry once a minute.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, Mutex, PoisonError,
    },
};

use metrics::{Counter, CounterFn, Gauge, GaugeFn, Key};
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
static RECORDED: LazyLock<Mutex<HashMap<Key, Arc<Periodic>>>> = LazyLock::new(Default::default);

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

fn recorded(key: &Key, kind: Kind) -> Arc<Periodic> {
    let mut recorded = RECORDED.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(periodic) = recorded.get(key) {
        return periodic.clone();
    }

    let periodic = Arc::new(Periodic::new(kind));
    recorded.insert(key.clone(), periodic.clone());
    periodic
}

/// Sends each listed counter's total since the previous call, and each listed gauge's latest
/// value.
pub(crate) fn flush() {
    let recorded = RECORDED.lock().unwrap_or_else(PoisonError::into_inner);

    for (key, periodic) in recorded.iter() {
        let name = key.name().to_owned();
        let labels = key
            .labels()
            .map(|label| (label.key().to_owned(), label.value().to_owned()));

        match &periodic.kind {
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
                    gauge = gauge.unit(unit.clone());
                }
                for (key, value) in labels {
                    gauge = gauge.attribute(key, value);
                }
                gauge.capture();
            }
        }
    }
}

enum Kind {
    Counter,
    Gauge(Option<Unit>),
}

/// A counter's total since the last [`flush`], or a gauge's latest value as `f64` bits.
struct Periodic {
    kind: Kind,
    value: AtomicU64,
}

impl Periodic {
    fn new(kind: Kind) -> Self {
        // A gauge is NaN until it is first set, so `flush` skips it.
        let value = match kind {
            Kind::Counter => 0,
            Kind::Gauge(_) => f64::NAN.to_bits(),
        };

        Self {
            kind,
            value: AtomicU64::new(value),
        }
    }

    fn update_gauge(&self, update: impl Fn(f64) -> f64) {
        let _ = self
            .value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                Some(update(f64::from_bits(bits)).to_bits())
            });
    }
}

impl CounterFn for Periodic {
    fn increment(&self, value: u64) {
        self.value.fetch_add(value, Ordering::Relaxed);
    }

    // Zebra only increments the listed counters.
    fn absolute(&self, _value: u64) {}
}

impl GaugeFn for Periodic {
    fn increment(&self, value: f64) {
        self.update_gauge(|gauge| if gauge.is_nan() { value } else { gauge + value });
    }

    fn decrement(&self, value: f64) {
        self.update_gauge(|gauge| {
            if gauge.is_nan() {
                -value
            } else {
                gauge - value
            }
        });
    }

    fn set(&self, value: f64) {
        self.value.store(value.to_bits(), Ordering::Relaxed);
    }
}
