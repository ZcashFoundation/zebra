//! Shapes every metric for Sentry, which stores each value it receives.
//!
//! Recording a metric only updates atomics in a sharded registry: there is no lock on a kept
//! handle, no allocation for an existing label set, and no call into the Sentry SDK. Once per
//! [`FLUSH_INTERVAL`], [`SentryPolicy::flush`] turns what was recorded since the previous flush
//! into one batch of [`SentryMetric`]s and gives it to an adapter that sends it:
//!
//! - each counter's increase since the previous flush, for each label set that increased;
//! - each gauge's latest value, for each label set that has been set;
//! - at most [`HISTOGRAM_CAP`] of each histogram's values for each label set, spread evenly across
//!   the values recorded since the previous flush.
//!
//! So a flush sends at most one value per counter or gauge label set, and [`HISTOGRAM_CAP`] values
//! per histogram label set, however often each one was recorded.
//!
//! [Event attributes](super::event_labels) never create a label set. Histograms keep them beside
//! each value, and send them without their `event.` prefix. Counters and gauges drop them, because
//! the policy keeps their values for the life of the process.

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex, PoisonError},
    time::Duration,
};

use metrics::{
    atomics::AtomicU64, Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName,
    Label, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::{
    registry::{Registry, Storage},
    storage::AtomicBucket,
};

use super::event_labels::{split_event_labels, EVENT_LABEL_PREFIX};

/// How often the policy sends what was recorded to Sentry.
///
/// Each counter and gauge label set sends at most one value per interval, so the volume Sentry
/// stores is inversely proportional to this.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// The most values one histogram label set sends per [`FLUSH_INTERVAL`].
///
/// An initial sync records some histograms for every block, faster than Sentry's transport can
/// send them. Histograms recorded about once per block near the tip stay under this cap.
pub(crate) const HISTOGRAM_CAP: usize = 20;

/// A value for the adapter to send to Sentry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SentryMetric {
    /// How Sentry stores the value.
    pub(crate) metric_type: MetricType,
    /// The metric's name, as the code records it.
    pub(crate) name: String,
    pub(crate) value: f64,
    /// The unit the metric was described with.
    pub(crate) unit: Option<Unit>,
    /// The metric's labels, then its event attributes without their prefix.
    pub(crate) attributes: Vec<(String, AttributeValue)>,
}

/// How Sentry stores a [`SentryMetric`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricType {
    /// A counter's increase since the previous flush.
    Counter,
    /// A gauge's latest value.
    Gauge,
    /// One recorded histogram value.
    Distribution,
}

/// The value of a [`SentryMetric`] attribute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AttributeValue {
    String(String),
    /// An event attribute whose value is an integer, such as a block height, so that Sentry
    /// queries can compare and aggregate it.
    Integer(u64),
}

/// A [`Recorder`] that keeps every metric for [`SentryPolicy::flush`] to send to Sentry.
pub(crate) struct SentryPolicy {
    registry: Registry<Key, PolicyStorage>,

    /// The unit of each described metric, by name.
    units: Mutex<HashMap<String, Unit>>,

    /// Held for a whole [`flush`](Self::flush), so that the flush at shutdown waits for a periodic
    /// flush that has taken counter increases but not sent them yet.
    flushing: Mutex<()>,
}

thread_local! {
    /// Whether this thread is in a [`SentryPolicy::flush`], so that the panic hook's flush returns
    /// instead of waiting for a flush that panicked on the same thread.
    static IN_FLUSH: Cell<bool> = const { Cell::new(false) };
}

impl Default for SentryPolicy {
    fn default() -> Self {
        Self {
            registry: Registry::new(PolicyStorage),
            units: Mutex::default(),
            flushing: Mutex::default(),
        }
    }
}

impl SentryPolicy {
    /// Gives `send` everything recorded since the previous flush, as one batch.
    ///
    /// Builds the batch before calling `send`, so `send` can record metrics, and this can run from
    /// the panic hook.
    pub(crate) fn flush(&self, send: impl FnOnce(Vec<SentryMetric>)) {
        if IN_FLUSH.get() {
            return;
        }

        let _flushing = self.flushing.lock().unwrap_or_else(PoisonError::into_inner);
        IN_FLUSH.set(true);
        send(self.take_batch());
        IN_FLUSH.set(false);
    }

    /// Takes everything recorded since the previous call. Only [`flush`](Self::flush) calls this.
    fn take_batch(&self) -> Vec<SentryMetric> {
        let units = self.units.lock().unwrap_or_else(PoisonError::into_inner);
        let unit = |key: &Key| units.get(key.name()).copied();
        let mut batch = Vec::new();

        self.registry.visit_counters(|key, counter| {
            let increase = counter.take_increase();
            if increase > 0 {
                // One interval's increase is far below 2^53, so it converts to f64 exactly.
                let value = increase as f64;
                batch.push(metric(MetricType::Counter, key, value, unit(key), &[]));
            }
        });

        self.registry.visit_gauges(|key, gauge| {
            if let Some(value) = gauge.get() {
                batch.push(metric(MetricType::Gauge, key, value, unit(key), &[]));
            }
        });

        self.registry.visit_histograms(|key, distribution| {
            for (value, event_labels) in distribution.take_capped(HISTOGRAM_CAP) {
                let event_labels = event_labels.as_deref().unwrap_or_default();
                batch.push(metric(
                    MetricType::Distribution,
                    key,
                    value,
                    unit(key),
                    event_labels,
                ));
            }
        });

        batch
    }

    fn describe(&self, key: KeyName, unit: Option<Unit>) {
        if let Some(unit) = unit {
            self.units
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key.as_str().to_owned(), unit);
        }
    }
}

/// Returns the metric to send for `value` under `key`, with `event_labels` as attributes after
/// the key's labels.
fn metric(
    metric_type: MetricType,
    key: &Key,
    value: f64,
    unit: Option<Unit>,
    event_labels: &[Label],
) -> SentryMetric {
    let labels = key.labels().map(|label| {
        (
            label.key().to_owned(),
            AttributeValue::String(label.value().to_owned()),
        )
    });
    let event_attributes = event_labels.iter().map(|label| {
        let key = label.key();
        let key = key.strip_prefix(EVENT_LABEL_PREFIX).unwrap_or(key);
        let value = match label.value().parse() {
            Ok(integer) => AttributeValue::Integer(integer),
            Err(_) => AttributeValue::String(label.value().to_owned()),
        };
        (key.to_owned(), value)
    });

    SentryMetric {
        metric_type,
        name: key.name().to_owned(),
        value,
        unit,
        attributes: labels.chain(event_attributes).collect(),
    }
}

impl Recorder for SentryPolicy {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, _: SharedString) {
        self.describe(key, unit);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, _: SharedString) {
        self.describe(key, unit);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, _: SharedString) {
        self.describe(key, unit);
    }

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let register = |key: &Key| {
            self.registry
                .get_or_create_counter(key, |counter| Counter::from_arc(counter.clone()))
        };

        match split_event_labels(key) {
            Some((key, _)) => register(&key),
            None => register(key),
        }
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let register = |key: &Key| {
            self.registry
                .get_or_create_gauge(key, |gauge| Gauge::from_arc(gauge.clone()))
        };

        match split_event_labels(key) {
            Some((key, _)) => register(&key),
            None => register(key),
        }
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        match split_event_labels(key) {
            Some((key, event_labels)) => {
                let distribution = self.registry.get_or_create_histogram(&key, Arc::clone);
                Histogram::from_arc(Arc::new(WithEventLabels {
                    distribution,
                    event_labels: event_labels.into(),
                }))
            }
            None => self.registry.get_or_create_histogram(key, |distribution| {
                Histogram::from_arc(distribution.clone())
            }),
        }
    }
}

/// Creates the values the [`SentryPolicy`] registry keeps for each label set.
struct PolicyStorage;

impl Storage<Key> for PolicyStorage {
    type Counter = Arc<CounterValue>;
    type Gauge = Arc<GaugeValue>;
    type Histogram = Arc<Distribution>;

    fn counter(&self, _: &Key) -> Self::Counter {
        Arc::default()
    }

    fn gauge(&self, _: &Key) -> Self::Gauge {
        Arc::new(GaugeValue(AtomicU64::new(f64::NAN.to_bits())))
    }

    fn histogram(&self, _: &Key) -> Self::Histogram {
        Arc::default()
    }
}

/// A counter's total, and its total when the previous flush took its increase.
///
/// Keeping the previous total, instead of resetting the counter, makes counters set with
/// [`absolute`](CounterFn::absolute), such as `process_cpu_seconds_total`, send their increase.
#[derive(Default)]
struct CounterValue {
    total: AtomicU64,
    flushed: AtomicU64,
}

impl CounterValue {
    /// Returns the increase since the previous call.
    fn take_increase(&self) -> u64 {
        let total = self.total.load(Ordering::Relaxed);
        let flushed = self.flushed.swap(total, Ordering::Relaxed);
        total.saturating_sub(flushed)
    }
}

impl CounterFn for CounterValue {
    fn increment(&self, value: u64) {
        self.total.fetch_add(value, Ordering::Relaxed);
    }

    fn absolute(&self, value: u64) {
        self.total.fetch_max(value, Ordering::Relaxed);
    }
}

/// A gauge's latest value as `f64` bits, or NaN until it is first set.
struct GaugeValue(AtomicU64);

impl GaugeValue {
    /// Returns the gauge's value, unless it was never set or isn't finite, which Sentry can't
    /// store.
    fn get(&self) -> Option<f64> {
        Some(f64::from_bits(self.0.load(Ordering::Relaxed))).filter(|value| value.is_finite())
    }

    fn update(&self, change: impl Fn(f64) -> f64) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                // A gauge that was never set starts at zero, as it does in Prometheus.
                let current = Some(f64::from_bits(bits))
                    .filter(|value| !value.is_nan())
                    .unwrap_or_default();
                Some(change(current).to_bits())
            });
    }
}

impl GaugeFn for GaugeValue {
    fn increment(&self, value: f64) {
        self.update(|current| current + value);
    }

    fn decrement(&self, value: f64) {
        self.update(|current| current - value);
    }

    fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// The values one histogram label set recorded since the previous flush.
#[derive(Default)]
struct Distribution {
    values: AtomicBucket<f64>,
    /// Values recorded with event attributes, beside those attributes.
    with_event_labels: AtomicBucket<(f64, Arc<[Label]>)>,
}

impl Distribution {
    /// Takes the recorded values, and returns at most `cap` of the finite ones, spread evenly
    /// across them, with their event attributes.
    fn take_capped(&self, cap: usize) -> Vec<(f64, Option<Arc<[Label]>>)> {
        let mut recorded = Vec::new();
        self.values
            .clear_with(|values| recorded.extend(values.iter().map(|value| (*value, None))));
        self.with_event_labels.clear_with(|values| {
            recorded.extend(
                values
                    .iter()
                    .map(|(value, event_labels)| (*value, Some(event_labels.clone()))),
            )
        });
        recorded.retain(|(value, _)| value.is_finite());

        let len = recorded.len();
        let kept = len.min(cap);
        (0..kept)
            .map(|i| recorded[i * len / kept].clone())
            .collect()
    }
}

impl HistogramFn for Distribution {
    fn record(&self, value: f64) {
        self.values.push(value);
    }
}

/// A histogram handle that records its event attributes beside each value.
struct WithEventLabels {
    distribution: Arc<Distribution>,
    event_labels: Arc<[Label]>,
}

impl HistogramFn for WithEventLabels {
    fn record(&self, value: f64) {
        self.distribution
            .with_event_labels
            .push((value, self.event_labels.clone()));
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use metrics::with_local_recorder;

    use super::*;

    /// Returns the batch of one flush of `policy`.
    fn flush(policy: &SentryPolicy) -> Vec<SentryMetric> {
        let mut sent = Vec::new();
        policy.flush(|batch| sent = batch);
        sent
    }

    fn string(value: &str) -> AttributeValue {
        AttributeValue::String(value.to_owned())
    }

    #[test]
    fn counters_send_their_increase_once_per_flush() {
        let policy = SentryPolicy::default();

        with_local_recorder(&policy, || {
            metrics::describe_counter!("a.count", Unit::Count, "A count.");
            for _ in 0..3 {
                metrics::counter!("a.count", "kind" => "not_found").increment(1);
            }
        });

        assert_eq!(
            flush(&policy),
            [SentryMetric {
                metric_type: MetricType::Counter,
                name: "a.count".to_owned(),
                value: 3.0,
                unit: Some(Unit::Count),
                attributes: vec![("kind".to_owned(), string("not_found"))],
            }],
        );
        assert!(flush(&policy).is_empty(), "nothing new was counted");

        with_local_recorder(&policy, || {
            metrics::counter!("a.count", "kind" => "not_found").increment(2)
        });
        assert_eq!(
            flush(&policy)
                .iter()
                .map(|metric| metric.value)
                .collect::<Vec<_>>(),
            [2.0],
        );
    }

    #[test]
    fn absolute_counters_send_their_increase() {
        let policy = SentryPolicy::default();
        let cpu_seconds = |total| {
            with_local_recorder(&policy, || {
                metrics::counter!("process_cpu_seconds_total").absolute(total)
            })
        };
        let values =
            |batch: Vec<SentryMetric>| batch.iter().map(|metric| metric.value).collect::<Vec<_>>();

        cpu_seconds(100);
        assert_eq!(values(flush(&policy)), [100.0]);

        cpu_seconds(130);
        cpu_seconds(145);
        assert_eq!(values(flush(&policy)), [45.0]);

        cpu_seconds(145);
        assert!(flush(&policy).is_empty(), "the total didn't increase");
    }

    #[test]
    fn gauges_send_their_latest_value_once_set() {
        let policy = SentryPolicy::default();

        with_local_recorder(&policy, || {
            metrics::describe_gauge!("a.bytes", Unit::Bytes, "Some bytes.");
            metrics::gauge!("a.bytes").set(30.0);
            metrics::gauge!("a.bytes").set(90.0);
            // Registered, but never set.
            let _ = metrics::gauge!("a.unset");
            // Only incremented, so it counts from zero.
            metrics::gauge!("a.connections").increment(2.0);
            metrics::gauge!("a.connections").decrement(1.0);
        });

        let mut sent: Vec<_> = flush(&policy)
            .into_iter()
            .map(|metric| (metric.metric_type, metric.name, metric.value, metric.unit))
            .collect();
        sent.sort_by(|a, b| a.1.cmp(&b.1));

        let gauge = |name: &str, value, unit| (MetricType::Gauge, name.to_owned(), value, unit);
        assert_eq!(
            sent,
            [
                gauge("a.bytes", 90.0, Some(Unit::Bytes)),
                gauge("a.connections", 1.0, None),
            ],
        );
        assert_eq!(
            flush(&policy).len(),
            2,
            "gauges send their latest value at every flush",
        );
    }

    #[test]
    fn histograms_send_at_most_the_cap_spread_across_the_interval() {
        let policy = SentryPolicy::default();
        let cap = u32::try_from(HISTOGRAM_CAP).expect("the cap is small");
        let recorded = 10 * cap;

        with_local_recorder(&policy, || {
            metrics::describe_histogram!("a.duration_seconds", Unit::Seconds, "A duration.");
            let histogram = metrics::histogram!("a.duration_seconds", "result" => "success");
            for value in 0..recorded {
                histogram.record(f64::from(value));
            }
            metrics::histogram!("a.duration_seconds", "result" => "failure").record(f64::NAN);
            metrics::histogram!("b.duration_seconds").record(1.5);
        });

        let sent = flush(&policy);
        let values = |name: &str| -> Vec<f64> {
            sent.iter()
                .filter(|metric| metric.name == name)
                .map(|metric| metric.value)
                .collect()
        };

        let mut capped = values("a.duration_seconds");
        capped.sort_by(f64::total_cmp);
        capped.dedup();
        assert_eq!(capped.len(), HISTOGRAM_CAP, "distinct values up to the cap");
        assert!(
            capped[0] < f64::from(recorded / 4)
                && capped[HISTOGRAM_CAP - 1] >= f64::from(recorded * 3 / 4),
            "values are spread across the interval: {capped:?}",
        );
        assert!(sent
            .iter()
            .filter(|metric| metric.name == "a.duration_seconds")
            .all(|metric| metric.metric_type == MetricType::Distribution
                && metric.unit == Some(Unit::Seconds)
                && metric.attributes == [("result".to_owned(), string("success"))]));
        assert_eq!(values("b.duration_seconds"), [1.5]);
        assert!(flush(&policy).is_empty(), "histograms only send new values");
    }

    #[test]
    fn event_attributes_are_kept_on_histograms_without_creating_label_sets() {
        let policy = SentryPolicy::default();

        with_local_recorder(&policy, || {
            for height in [3_543_405_u32, 3_543_406] {
                metrics::histogram!(
                    "a.delay_seconds",
                    "event.chain.height" => height.to_string(),
                    "chain.network_upgrade" => "Nu7",
                    "event.block.hash" => "00ab",
                )
                .record(2.0);
            }
            metrics::counter!("a.count", "event.chain.height" => "1").increment(1);
            metrics::counter!("a.count", "event.chain.height" => "2").increment(1);
            metrics::gauge!("a.gauge", "event.chain.height" => "1").set(5.0);
        });

        let mut histogram_label_sets = 0;
        policy
            .registry
            .visit_histograms(|_, _| histogram_label_sets += 1);
        let mut counter_label_sets = 0;
        policy
            .registry
            .visit_counters(|_, _| counter_label_sets += 1);
        assert_eq!((histogram_label_sets, counter_label_sets), (1, 1));

        let sent = flush(&policy);
        let attributes = |name: &str| -> Vec<Vec<(String, AttributeValue)>> {
            sent.iter()
                .filter(|metric| metric.name == name)
                .map(|metric| metric.attributes.clone())
                .collect()
        };
        let delay = |height| {
            vec![
                ("chain.network_upgrade".to_owned(), string("Nu7")),
                ("chain.height".to_owned(), AttributeValue::Integer(height)),
                ("block.hash".to_owned(), string("00ab")),
            ]
        };

        let mut delays = attributes("a.delay_seconds");
        delays.sort_by_key(|attributes| format!("{attributes:?}"));
        assert_eq!(delays, [delay(3_543_405), delay(3_543_406)]);
        assert_eq!(attributes("a.count"), [vec![]]);
        assert_eq!(
            sent.iter()
                .find(|metric| metric.name == "a.count")
                .map(|metric| metric.value),
            Some(2.0),
            "counts with different event attributes add up",
        );
        assert_eq!(attributes("a.gauge"), [vec![]]);
    }

    #[test]
    fn flush_waits_for_a_flush_in_progress() {
        let policy = Arc::new(SentryPolicy::default());
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        // Stands in for a periodic flush that has taken the increases but not sent them yet.
        let in_progress = thread::spawn({
            let policy = policy.clone();
            move || {
                let _flushing = policy
                    .flushing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                locked_tx.send(()).expect("the test thread is waiting");
                release_rx
                    .recv()
                    .expect("the test thread releases the flush");
            }
        });
        locked_rx
            .recv()
            .expect("the other thread holds the flush lock");

        let (flushed_tx, flushed_rx) = mpsc::channel();
        let flushing = thread::spawn({
            let policy = policy.clone();
            move || {
                policy.flush(drop);
                flushed_tx.send(()).expect("the test thread is waiting");
            }
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
        let policy = SentryPolicy::default();

        // A panic during a flush runs the panic hook's flush on the same thread.
        let _flushing = policy
            .flushing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        IN_FLUSH.set(true);

        policy.flush(|_| panic!("a reentered flush doesn't send"));

        IN_FLUSH.set(false);
    }
}
