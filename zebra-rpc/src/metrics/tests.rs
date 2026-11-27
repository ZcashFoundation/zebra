//! Test recorder for asserting `zebra-rpc` metrics.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder,
    SharedString, Unit,
};

type SeriesKey = (String, Vec<(String, String)>);

fn series_key(key: &Key) -> SeriesKey {
    let mut labels: Vec<(String, String)> = key
        .labels()
        .map(|label| (label.key().to_owned(), label.value().to_owned()))
        .collect();
    labels.sort();
    (key.name().to_owned(), labels)
}

fn lookup<T>(
    map: &Mutex<BTreeMap<SeriesKey, Arc<T>>>,
    name: &str,
    labels: &[(&str, &str)],
) -> Option<Arc<T>> {
    map.lock()
        .expect("test metrics map lock is never poisoned")
        .get(&TestMetricsRecorder::key(name, labels))
        .cloned()
}

fn register<T: Default>(map: &Mutex<BTreeMap<SeriesKey, Arc<T>>>, key: &Key) -> Arc<T> {
    map.lock()
        .expect("test metrics map lock is never poisoned")
        .entry(series_key(key))
        .or_default()
        .clone()
}

#[derive(Default)]
struct CounterState(AtomicU64);

impl CounterFn for CounterState {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::SeqCst);
    }

    fn absolute(&self, value: u64) {
        self.0.fetch_max(value, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct GaugeState {
    value: Mutex<f64>,
    max: Mutex<Option<f64>>,
    updates: AtomicU64,
}

impl GaugeState {
    fn update(&self, apply: impl FnOnce(&mut f64)) {
        let mut guard = self
            .value
            .lock()
            .expect("recording never panics while holding the gauge lock");
        apply(&mut guard);
        let updated = *guard;
        drop(guard);

        let mut max = self
            .max
            .lock()
            .expect("recording never panics while holding the gauge max lock");
        *max = Some(max.map_or(updated, |prev| prev.max(updated)));
        self.updates.fetch_add(1, Ordering::SeqCst);
    }
}

impl GaugeFn for GaugeState {
    fn increment(&self, value: f64) {
        self.update(|current| *current += value);
    }

    fn decrement(&self, value: f64) {
        self.update(|current| *current -= value);
    }

    fn set(&self, value: f64) {
        self.update(|current| *current = value);
    }
}

#[derive(Default)]
struct HistogramState(Mutex<Vec<f64>>);

impl HistogramFn for HistogramState {
    fn record(&self, value: f64) {
        self.0
            .lock()
            .expect("recording never panics while holding the histogram lock")
            .push(value);
    }
}

/// In-memory `metrics::Recorder` for `zebra-rpc` unit and integration tests.
#[derive(Default)]
pub(crate) struct TestMetricsRecorder {
    counters: Mutex<BTreeMap<SeriesKey, Arc<CounterState>>>,
    gauges: Mutex<BTreeMap<SeriesKey, Arc<GaugeState>>>,
    histograms: Mutex<BTreeMap<SeriesKey, Arc<HistogramState>>>,
}

impl TestMetricsRecorder {
    fn key(name: &str, labels: &[(&str, &str)]) -> SeriesKey {
        let mut labels: Vec<(String, String)> = labels
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        labels.sort();
        (name.to_owned(), labels)
    }

    /// Returns the total recorded for `name` with `labels`, or `None` if never registered.
    pub(crate) fn counter(&self, name: &str, labels: &[(&str, &str)]) -> Option<u64> {
        lookup(&self.counters, name, labels).map(|state| state.0.load(Ordering::SeqCst))
    }

    /// Returns the current gauge value for `name` with `labels`, or `None` if never registered.
    pub(crate) fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        lookup(&self.gauges, name, labels).map(|state| {
            *state
                .value
                .lock()
                .expect("gauge value lock is never poisoned")
        })
    }

    /// Returns the maximum observed gauge value for `name` with `labels`.
    pub(crate) fn gauge_max(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        lookup(&self.gauges, name, labels)
            .and_then(|state| *state.max.lock().expect("gauge max lock is never poisoned"))
    }

    /// Returns how many times the gauge for `name` with `labels` was updated.
    pub(crate) fn gauge_updates(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
        lookup(&self.gauges, name, labels).map_or(0, |state| state.updates.load(Ordering::SeqCst))
    }

    /// Returns all histogram samples recorded for `name` with `labels`.
    pub(crate) fn histogram(&self, name: &str, labels: &[(&str, &str)]) -> Vec<f64> {
        lookup(&self.histograms, name, labels)
            .map(|state| {
                state
                    .0
                    .lock()
                    .expect("histogram samples lock is never poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }
}

impl Recorder for TestMetricsRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(register(&self.counters, key))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(register(&self.gauges, key))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::from_arc(register(&self.histograms, key))
    }
}
