//! Shapes every metric for Sentry, which stores each value it receives.
//!
//! Recording a metric only updates atomics in a sharded registry: there is no lock on a kept
//! handle, no allocation for an existing label set, and no call into the Sentry SDK. Once per
//! [`FLUSH_INTERVAL`], [`SentryPolicy::flush`] turns what was recorded into one batch of
//! [`SentryMetric`]s and gives it to an adapter that sends it:
//!
//! - each counter's increase since it was last sent, for each label set that increased;
//! - each gauge's latest value, for each label set whose value changed since it was last sent, or
//!   that hasn't been sent for [`GAUGE_REFRESH_FLUSHES`] flushes;
//! - at most [`HISTOGRAM_CAP`] of each histogram's values for each label set, spread evenly across
//!   the values recorded since the previous flush.
//!
//! A batch holds at most [`FLUSH_BUDGET`] metrics. When more are due, counters go first, then
//! gauges that changed, then gauges due for a refresh, then histogram values. Counters and gauges
//! that don't fit stay due and take turns in later flushes, so a counter's increase is delayed,
//! not lost. Histogram values that don't fit are dropped.
//!
//! The policy keeps at most [`LABEL_SET_CAP`] label sets for each metric name, and doesn't send
//! the values recorded for any others, so a label that keeps taking new values can't grow the
//! policy's memory or volume without bound. Other backends keep every label set.
//!
//! The `zebrad.sentry.metrics.dropped.total` counter counts the due values left out of a full
//! flush, and the values recorded for label sets over the cap. The policy records it through the
//! `metrics` facade, so every backend receives it, and logs a warning when it starts dropping
//! values.
//!
//! [Event attributes](super::event_labels) never create a label set. Histograms keep them beside
//! each value, and send them without their `event.` prefix. Counters and gauges drop them, because
//! the policy keeps their values for the life of the process.

use std::{
    cell::Cell,
    collections::HashMap,
    mem,
    ops::AddAssign,
    sync::{atomic::Ordering, Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
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

/// The most metrics the Sentry SDK puts in one envelope. Its batcher queues the envelope for
/// sending as soon as it holds this many, on the thread that captured the last one.
pub(crate) const ENVELOPE_METRICS: usize = 100;

/// How many envelopes the Sentry SDK's transport queues for sending. Events, logs, sessions, and
/// metrics share this queue, and the SDK drops an envelope that arrives while it is full.
pub(crate) const TRANSPORT_QUEUE_ENVELOPES: usize = 30;

/// The most metrics one flush sends.
///
/// 15 envelopes of 100 metrics is half of the 30-envelope transport queue, so even if the
/// transport sends nothing while a flush is queued, 15 envelopes stay free for events, logs, and
/// sessions.
pub(crate) const FLUSH_BUDGET: usize = TRANSPORT_QUEUE_ENVELOPES / 2 * ENVELOPE_METRICS;

/// The most label sets the policy keeps for one metric name.
///
/// Zebra's labels take a fixed set of values, and every metric stays under this cap on a busy
/// node. A metric that reaches it has a label that keeps taking new values.
pub(crate) const LABEL_SET_CAP: usize = 256;

/// How many flushes an unchanged gauge waits before it is sent again, so that Sentry charts show
/// steady values: every 5 minutes.
pub(crate) const GAUGE_REFRESH_FLUSHES: u64 = 5;

/// The least time between two warnings that the policy dropped values.
const DROP_WARNING_INTERVAL: Duration = Duration::from_secs(60 * 60);

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
    /// A counter's increase since it was last sent.
    Counter,
    /// A gauge's latest value.
    Gauge,
    /// One recorded histogram value.
    Distribution,
}

/// The value of a [`SentryMetric`] attribute.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum AttributeValue {
    String(String),
    /// An event attribute whose value is an integer, such as a block height, so that Sentry
    /// queries can compare and aggregate it.
    Integer(u64),
}

/// A [`Recorder`] that keeps every metric for [`SentryPolicy::flush`] to send to Sentry.
pub(crate) struct SentryPolicy {
    registry: Registry<Key, PolicyStorage>,

    /// How many label sets the registry keeps for each metric name. Only registering a label set
    /// the registry doesn't have locks this.
    label_sets: Mutex<LabelSets>,

    /// Counts the values recorded for the label sets the registry doesn't keep.
    untracked: Arc<Untracked>,

    /// The unit of each described metric, by name.
    units: Mutex<HashMap<String, Unit>>,

    /// Held for a whole [`flush`](Self::flush), so that the flush at shutdown waits for a periodic
    /// flush that has taken counter increases but not sent them yet.
    flushing: Mutex<FlushState>,
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
            label_sets: Mutex::default(),
            untracked: Arc::default(),
            units: Mutex::default(),
            flushing: Mutex::default(),
        }
    }
}

impl SentryPolicy {
    /// Gives `send` the metrics that are due, as one batch of at most [`FLUSH_BUDGET`] metrics.
    ///
    /// Builds the batch before calling `send`, so `send` can record metrics, and this can run from
    /// the panic hook.
    pub(crate) fn flush(&self, send: impl FnOnce(Vec<SentryMetric>)) {
        if IN_FLUSH.get() {
            return;
        }

        let mut state = self.flushing.lock().unwrap_or_else(PoisonError::into_inner);
        IN_FLUSH.set(true);

        let (batch, over_budget) = self.take_batch(&mut state);
        let dropped = Dropped {
            flush_budget: over_budget,
            label_set_cap: self.untracked.take(),
        };
        let capped = mem::take(&mut self.lock_label_sets().newly_capped);

        dropped.record();
        if let Some((dropped, capped)) = state.warning.add(dropped, capped, Instant::now()) {
            tracing::warn!(
                flush_budget = dropped.flush_budget,
                label_set_cap = dropped.label_set_cap,
                capped_metrics = ?capped,
                "some metric values weren't sent to Sentry; \
                 zebrad.sentry.metrics.dropped.total counts them by reason",
            );
        }

        send(batch);
        IN_FLUSH.set(false);
    }

    /// Takes the metrics that are due, and returns at most [`FLUSH_BUDGET`] of them, with how many
    /// didn't fit. Only [`flush`](Self::flush) calls this.
    fn take_batch(&self, state: &mut FlushState) -> (Vec<SentryMetric>, u64) {
        state.flushes += 1;
        let flush = state.flushes;

        let mut counters = Vec::new();
        self.registry.visit_counters(|key, counter| {
            if counter.has_increase() {
                counters.push((key.clone(), counter.clone()));
            }
        });

        let mut changed_gauges = Vec::new();
        let mut unchanged_gauges = Vec::new();
        self.registry
            .visit_gauges(|key, gauge| match gauge.due(flush) {
                Some(GaugeDue::Changed) => changed_gauges.push((key.clone(), gauge.clone())),
                Some(GaugeDue::Refresh) => unchanged_gauges.push((key.clone(), gauge.clone())),
                None => {}
            });

        let mut histograms = Vec::new();
        self.registry.visit_histograms(|key, distribution| {
            let values = distribution.take_capped(HISTOGRAM_CAP);
            if !values.is_empty() {
                histograms.push((key.clone(), values));
            }
        });

        let units = self.units.lock().unwrap_or_else(PoisonError::into_inner);
        let unit = |key: &Key| units.get(key.name()).copied();
        let mut room = FLUSH_BUDGET;
        let mut over_budget = 0;
        let mut batch = Vec::new();
        let [next_counter, next_changed_gauge, next_unchanged_gauge] = &mut state.next;

        let (counters, left_out) = take_turns(counters, &mut room, next_counter);
        over_budget += left_out;
        for (key, counter) in counters {
            // One interval's increase is far below 2^53, so it converts to f64 exactly.
            let value = counter.take_increase() as f64;
            batch.push(metric(MetricType::Counter, &key, value, unit(&key), &[]));
        }

        for (gauges, next) in [
            (changed_gauges, next_changed_gauge),
            (unchanged_gauges, next_unchanged_gauge),
        ] {
            let (gauges, left_out) = take_turns(gauges, &mut room, next);
            over_budget += left_out;
            for (key, gauge) in gauges {
                if let Some(value) = gauge.take_unsent(flush) {
                    batch.push(metric(MetricType::Gauge, &key, value, unit(&key), &[]));
                }
            }
        }

        let recorded: usize = histograms.iter().map(|(_, values)| values.len()).sum();
        let kept = recorded.min(room);
        over_budget += recorded - kept;
        // Spread across every label set's values, so each label set keeps its share.
        let mut kept_indexes = (0..kept).map(|i| i * recorded / kept).peekable();
        let values = histograms
            .iter()
            .flat_map(|(key, values)| values.iter().map(move |value| (key, value)));
        for (index, (key, (value, event_labels))) in values.enumerate() {
            if kept_indexes.next_if_eq(&index).is_some() {
                let event_labels = event_labels.as_deref().unwrap_or_default();
                batch.push(metric(
                    MetricType::Distribution,
                    key,
                    *value,
                    unit(key),
                    event_labels,
                ));
            }
        }

        (batch, u64::try_from(over_budget).unwrap_or(u64::MAX))
    }

    /// Returns the value the registry keeps for `key` from `get`, or creates it with `create`
    /// unless `key`'s name already has [`LABEL_SET_CAP`] label sets.
    fn tracked<T>(
        &self,
        key: &Key,
        get: impl Fn(&Key) -> Option<T>,
        create: impl FnOnce(&Key) -> T,
    ) -> Option<T> {
        if let Some(value) = get(key) {
            return Some(value);
        }

        let mut label_sets = self.lock_label_sets();
        // Another thread may have created it while this one waited for the lock.
        if let Some(value) = get(key) {
            return Some(value);
        }

        label_sets.try_add(key.name()).then(|| create(key))
    }

    fn lock_label_sets(&self) -> MutexGuard<'_, LabelSets> {
        self.label_sets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

/// Takes the `due` metrics that fit in `room`, starting at `*next` and wrapping around, and moves
/// `*next` past them, so that metrics left out go first in a later flush. Returns the metrics
/// taken, and how many were left out.
fn take_turns<T>(mut due: Vec<T>, room: &mut usize, next: &mut usize) -> (Vec<T>, usize) {
    let len = due.len();
    let taken = len.min(*room);
    *room -= taken;

    if taken < len {
        let start = *next % len;
        due.rotate_left(start);
        due.truncate(taken);
        *next = start + taken;
    }

    (due, len - taken)
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
            let counter = self.tracked(
                key,
                |key| self.registry.get_counter(key),
                |key| self.registry.get_or_create_counter(key, Arc::clone),
            );
            match counter {
                Some(counter) => Counter::from_arc(counter),
                None => Counter::from_arc(self.untracked.clone()),
            }
        };

        match split_event_labels(key) {
            Some((key, _)) => register(&key),
            None => register(key),
        }
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let register = |key: &Key| {
            let gauge = self.tracked(
                key,
                |key| self.registry.get_gauge(key),
                |key| self.registry.get_or_create_gauge(key, Arc::clone),
            );
            match gauge {
                Some(gauge) => Gauge::from_arc(gauge),
                None => Gauge::from_arc(self.untracked.clone()),
            }
        };

        match split_event_labels(key) {
            Some((key, _)) => register(&key),
            None => register(key),
        }
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        let distribution = |key: &Key| {
            self.tracked(
                key,
                |key| self.registry.get_histogram(key),
                |key| self.registry.get_or_create_histogram(key, Arc::clone),
            )
        };

        match split_event_labels(key) {
            Some((key, event_labels)) => match distribution(&key) {
                Some(distribution) => Histogram::from_arc(Arc::new(WithEventLabels {
                    distribution,
                    event_labels: event_labels.into(),
                })),
                None => Histogram::from_arc(self.untracked.clone()),
            },
            None => match distribution(key) {
                Some(distribution) => Histogram::from_arc(distribution),
                None => Histogram::from_arc(self.untracked.clone()),
            },
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
        Arc::default()
    }

    fn histogram(&self, _: &Key) -> Self::Histogram {
        Arc::default()
    }
}

/// How many label sets the [`SentryPolicy`] registry keeps for each metric name.
#[derive(Default)]
struct LabelSets {
    by_name: HashMap<String, NameLabelSets>,
    /// The names that reached [`LABEL_SET_CAP`] since the last flush, once each.
    newly_capped: Vec<String>,
}

#[derive(Default)]
struct NameLabelSets {
    kept: usize,
    capped: bool,
}

impl LabelSets {
    /// Adds a label set for the metric `name`, and returns whether the name had room for it.
    fn try_add(&mut self, name: &str) -> bool {
        let Some(label_sets) = self.by_name.get_mut(name) else {
            self.by_name.insert(
                name.to_owned(),
                NameLabelSets {
                    kept: 1,
                    capped: false,
                },
            );
            return true;
        };

        if label_sets.kept < LABEL_SET_CAP {
            label_sets.kept += 1;
            return true;
        }

        if !label_sets.capped {
            label_sets.capped = true;
            self.newly_capped.push(name.to_owned());
        }
        false
    }
}

/// The state that each flush updates.
#[derive(Default)]
struct FlushState {
    /// How many flushes have taken a batch.
    flushes: u64,
    /// Where counters, changed gauges, and unchanged gauges start taking turns when they don't
    /// all fit in a flush.
    next: [usize; 3],
    warning: DropWarning,
}

/// The values the policy didn't send, by why.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Dropped {
    /// Values that were due, but didn't fit in a flush's [`FLUSH_BUDGET`].
    flush_budget: u64,
    /// Values recorded for label sets over [`LABEL_SET_CAP`].
    label_set_cap: u64,
}

impl Dropped {
    /// Counts these values in the installed recorder.
    fn record(&self) {
        for (reason, values) in [
            ("flush_budget", self.flush_budget),
            ("label_set_cap", self.label_set_cap),
        ] {
            if values > 0 {
                metrics::counter!("zebrad.sentry.metrics.dropped.total", "reason" => reason)
                    .increment(values);
            }
        }
    }
}

impl AddAssign for Dropped {
    fn add_assign(&mut self, other: Self) {
        self.flush_budget = self.flush_budget.saturating_add(other.flush_budget);
        self.label_set_cap = self.label_set_cap.saturating_add(other.label_set_cap);
    }
}

/// Decides when the policy warns that it dropped values.
#[derive(Default)]
struct DropWarning {
    /// When the policy last warned.
    last: Option<Instant>,
    /// The values dropped since then.
    dropped: Dropped,
    /// The metrics that reached [`LABEL_SET_CAP`] since then.
    capped: Vec<String>,
}

impl DropWarning {
    /// Adds what a flush at `now` dropped, and returns everything dropped since the last warning
    /// if a warning is due: when drops start, then at most once per [`DROP_WARNING_INTERVAL`].
    fn add(
        &mut self,
        dropped: Dropped,
        capped: Vec<String>,
        now: Instant,
    ) -> Option<(Dropped, Vec<String>)> {
        self.dropped += dropped;
        self.capped.extend(capped);

        let nothing_dropped = self.dropped == Dropped::default() && self.capped.is_empty();
        let warned_recently = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < DROP_WARNING_INTERVAL);
        if nothing_dropped || warned_recently {
            return None;
        }

        self.last = Some(now);
        Some((mem::take(&mut self.dropped), mem::take(&mut self.capped)))
    }
}

/// Stands in for every label set over [`LABEL_SET_CAP`], and counts the values recorded for them.
#[derive(Default)]
struct Untracked(AtomicU64);

impl Untracked {
    fn drop_value(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns how many values were recorded since the previous call.
    fn take(&self) -> u64 {
        self.0.swap(0, Ordering::Relaxed)
    }
}

impl CounterFn for Untracked {
    fn increment(&self, _: u64) {
        self.drop_value();
    }

    fn absolute(&self, _: u64) {
        self.drop_value();
    }
}

impl GaugeFn for Untracked {
    fn increment(&self, _: f64) {
        self.drop_value();
    }

    fn decrement(&self, _: f64) {
        self.drop_value();
    }

    fn set(&self, _: f64) {
        self.drop_value();
    }
}

impl HistogramFn for Untracked {
    fn record(&self, _: f64) {
        self.drop_value();
    }
}

/// A counter's total, and its total when a flush last took its increase.
///
/// Keeping the previous total, instead of resetting the counter, makes counters set with
/// [`absolute`](CounterFn::absolute), such as `process_cpu_seconds_total`, send their increase.
#[derive(Default)]
struct CounterValue {
    total: AtomicU64,
    flushed: AtomicU64,
}

impl CounterValue {
    fn has_increase(&self) -> bool {
        self.total.load(Ordering::Relaxed) > self.flushed.load(Ordering::Relaxed)
    }

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

/// A gauge's latest value, and the value and flush it was last sent at. Values are `f64` bits.
///
/// Only flushes, which hold [`SentryPolicy::flushing`], use `sent` and `sent_at`.
struct GaugeValue {
    /// The latest value, or NaN until the gauge is first set.
    value: AtomicU64,
    /// The value last sent, or NaN until the gauge is first sent.
    sent: AtomicU64,
    /// The flush that last sent the gauge.
    sent_at: AtomicU64,
}

/// Why a flush sends a gauge.
enum GaugeDue {
    Changed,
    Refresh,
}

impl Default for GaugeValue {
    fn default() -> Self {
        Self {
            value: AtomicU64::new(f64::NAN.to_bits()),
            sent: AtomicU64::new(f64::NAN.to_bits()),
            sent_at: AtomicU64::new(0),
        }
    }
}

impl GaugeValue {
    /// Returns the gauge's value, unless it was never set or isn't finite, which Sentry can't
    /// store.
    fn get(&self) -> Option<f64> {
        Some(f64::from_bits(self.value.load(Ordering::Relaxed))).filter(|value| value.is_finite())
    }

    /// Returns why the flush numbered `flush` sends this gauge, or `None` if it doesn't.
    fn due(&self, flush: u64) -> Option<GaugeDue> {
        let value = self.get()?;

        if value.to_bits() != self.sent.load(Ordering::Relaxed) {
            Some(GaugeDue::Changed)
        } else if flush.saturating_sub(self.sent_at.load(Ordering::Relaxed))
            >= GAUGE_REFRESH_FLUSHES
        {
            Some(GaugeDue::Refresh)
        } else {
            None
        }
    }

    /// Returns the value for the flush numbered `flush` to send, and remembers it as sent.
    fn take_unsent(&self, flush: u64) -> Option<f64> {
        let value = self.get()?;
        self.sent.store(value.to_bits(), Ordering::Relaxed);
        self.sent_at.store(flush, Ordering::Relaxed);
        Some(value)
    }

    fn update(&self, change: impl Fn(f64) -> f64) {
        let _ = self
            .value
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
        self.value.store(value.to_bits(), Ordering::Relaxed);
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
        assert!(
            flush(&policy).is_empty(),
            "unchanged gauges wait for their refresh",
        );
    }

    #[test]
    fn unchanged_gauges_are_sent_again_only_at_each_refresh() {
        let policy = SentryPolicy::default();
        let set = |value| with_local_recorder(&policy, || metrics::gauge!("a.peers").set(value));
        let values =
            |batch: Vec<SentryMetric>| batch.iter().map(|metric| metric.value).collect::<Vec<_>>();

        set(8.0);
        assert_eq!(values(flush(&policy)), [8.0], "a new gauge is sent");

        for _ in 1..GAUGE_REFRESH_FLUSHES {
            set(8.0);
            assert!(
                flush(&policy).is_empty(),
                "an unchanged gauge waits for its refresh",
            );
        }
        assert_eq!(
            values(flush(&policy)),
            [8.0],
            "an unchanged gauge is sent at its refresh",
        );

        set(9.0);
        assert_eq!(
            values(flush(&policy)),
            [9.0],
            "a changed gauge is sent at the next flush",
        );
        assert!(flush(&policy).is_empty());
    }

    fn count(values: usize) -> f64 {
        f64::from(u32::try_from(values).expect("test counts are small"))
    }

    /// Returns the values the batch reports as dropped for `reason`.
    fn dropped(batch: &[SentryMetric], reason: &str) -> f64 {
        batch
            .iter()
            .filter(|metric| {
                metric.name == "zebrad.sentry.metrics.dropped.total"
                    && metric.attributes == [("reason".to_owned(), string(reason))]
            })
            .map(|metric| metric.value)
            .sum()
    }

    /// Registers `label_sets` label sets with `register`, spread over the metrics `prefix0`,
    /// `prefix1`, and so on, at most [`LABEL_SET_CAP`] per name, and calls `record` with each.
    fn record_label_sets<T>(
        policy: &SentryPolicy,
        prefix: &str,
        label_sets: usize,
        register: impl Fn(String, String) -> T,
        record: impl Fn(T),
    ) {
        with_local_recorder(policy, || {
            for label_set in 0..label_sets {
                let name = format!("{prefix}{}", label_set / LABEL_SET_CAP);
                record(register(name, label_set.to_string()));
            }
        });
    }

    #[test]
    fn a_full_flush_sends_counters_then_changed_gauges_then_histogram_values() {
        let policy = SentryPolicy::default();
        let (counters, gauges, histograms) = (600, 600, 400);
        let records_per_histogram = 2 * HISTOGRAM_CAP;

        record_label_sets(
            &policy,
            "c",
            counters,
            |name, label| metrics::counter!(name, "i" => label),
            |counter| counter.increment(1),
        );
        record_label_sets(
            &policy,
            "g",
            gauges,
            |name, label| metrics::gauge!(name, "i" => label),
            |gauge| gauge.set(1.0),
        );
        record_label_sets(
            &policy,
            "h",
            histograms,
            |name, label| metrics::histogram!(name, "i" => label),
            |histogram| {
                for _ in 0..records_per_histogram {
                    histogram.record(1.0);
                }
            },
        );

        let sent = with_local_recorder(&policy, || flush(&policy));
        let types: Vec<MetricType> = sent.iter().map(|metric| metric.metric_type).collect();
        let histogram_room = FLUSH_BUDGET - counters - gauges;

        assert_eq!(sent.len(), FLUSH_BUDGET, "a full flush sends its budget");
        assert!(
            types[..counters].iter().all(|t| *t == MetricType::Counter)
                && types[counters..counters + gauges]
                    .iter()
                    .all(|t| *t == MetricType::Gauge)
                && types[counters + gauges..]
                    .iter()
                    .all(|t| *t == MetricType::Distribution),
            "counters, then gauges, then histogram values",
        );

        let mut sampled_label_sets: Vec<_> = sent[counters + gauges..]
            .iter()
            .map(|metric| (&metric.name, &metric.attributes))
            .collect();
        sampled_label_sets.dedup();
        assert_eq!(
            sampled_label_sets.len(),
            histogram_room.min(histograms),
            "histogram values are spread across label sets",
        );

        let next = flush(&policy);
        assert_eq!(
            dropped(&next, "flush_budget"),
            count(histograms * HISTOGRAM_CAP - histogram_room),
            "every histogram value left out is counted",
        );
        assert_eq!(dropped(&next, "label_set_cap"), 0.0);
    }

    #[test]
    fn counters_left_out_of_a_full_flush_take_turns_and_keep_their_increase() {
        let policy = SentryPolicy::default();
        let label_sets = FLUSH_BUDGET + FLUSH_BUDGET / 3;
        let increment = |policy: &SentryPolicy| {
            record_label_sets(
                policy,
                "c",
                label_sets,
                |name, label| metrics::counter!(name, "i" => label),
                |counter| counter.increment(1),
            )
        };

        let mut sent = Vec::new();
        increment(&policy);
        let first = flush(&policy);
        assert_eq!(first.len(), FLUSH_BUDGET);
        sent.extend(first);

        increment(&policy);
        for _ in 0..3 {
            sent.extend(flush(&policy));
        }

        let mut totals: HashMap<_, f64> = HashMap::new();
        for metric in &sent {
            *totals
                .entry((metric.name.clone(), format!("{:?}", metric.attributes)))
                .or_default() += metric.value;
        }
        assert_eq!(totals.len(), label_sets, "every label set gets its turn");
        assert!(
            totals.values().all(|total| *total == 2.0),
            "counters left out keep their increase for a later flush",
        );
    }

    #[test]
    fn label_sets_over_the_cap_are_neither_kept_nor_sent() {
        let policy = SentryPolicy::default();
        let over = 10;

        with_local_recorder(&policy, || {
            for label_set in 0..LABEL_SET_CAP + over {
                metrics::counter!("a.count", "i" => label_set.to_string()).increment(1);
            }
            let kept_handle = metrics::counter!("a.count", "i" => "kept_handle");
            for _ in 0..5 {
                kept_handle.increment(1);
            }
            for label_set in 0..=LABEL_SET_CAP {
                metrics::gauge!("a.gauge", "i" => label_set.to_string()).set(1.0);
                metrics::histogram!(
                    "a.delay_seconds",
                    "i" => label_set.to_string(),
                    "event.chain.height" => label_set.to_string(),
                )
                .record(1.0);
            }
            // A label set the policy keeps still records after the name reaches the cap.
            metrics::counter!("a.count", "i" => "0").increment(1);
            metrics::counter!("b.count", "i" => "0").increment(1);
        });

        let mut a_label_sets = 0;
        policy.registry.visit_counters(|key, _| {
            if key.name() == "a.count" {
                a_label_sets += 1;
            }
        });
        assert_eq!(a_label_sets, LABEL_SET_CAP);

        let sent = with_local_recorder(&policy, || flush(&policy));
        let total = |name: &str| -> (usize, f64) {
            let values: Vec<f64> = sent
                .iter()
                .filter(|metric| metric.name == name)
                .map(|metric| metric.value)
                .collect();
            (values.len(), values.iter().sum())
        };
        assert_eq!(total("a.count"), (LABEL_SET_CAP, count(LABEL_SET_CAP + 1)));
        assert_eq!(total("a.gauge").0, LABEL_SET_CAP);
        assert_eq!(total("a.delay_seconds").0, LABEL_SET_CAP);
        assert_eq!(total("b.count"), (1, 1.0), "the cap is for each name");

        let next = flush(&policy);
        assert_eq!(
            dropped(&next, "label_set_cap"),
            count(over + 5 + 1 + 1),
            "every value recorded for a label set over the cap is counted",
        );
    }

    #[test]
    fn drop_warnings_start_with_the_first_drop_and_repeat_at_most_hourly() {
        let mut warning = DropWarning::default();
        let start = Instant::now();
        let over_budget = |values| Dropped {
            flush_budget: values,
            label_set_cap: 0,
        };
        let capped = || vec!["a.count".to_owned()];

        assert_eq!(warning.add(Dropped::default(), Vec::new(), start), None);
        assert_eq!(
            warning.add(over_budget(3), Vec::new(), start),
            Some((over_budget(3), Vec::new())),
            "drops that start are logged at once",
        );
        assert_eq!(
            warning.add(over_budget(4), capped(), start + FLUSH_INTERVAL),
            None,
        );

        let an_hour_later = start + DROP_WARNING_INTERVAL;
        assert_eq!(
            warning.add(over_budget(5), Vec::new(), an_hour_later),
            Some((over_budget(9), capped())),
            "the next warning has everything dropped since the last one",
        );

        let later = an_hour_later + 2 * DROP_WARNING_INTERVAL;
        assert_eq!(warning.add(Dropped::default(), Vec::new(), later), None);
        assert_eq!(
            warning.add(over_budget(1), Vec::new(), later),
            Some((over_budget(1), Vec::new())),
            "drops that start again are logged at once",
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
