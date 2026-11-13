//! Shapes every metric for Sentry, which stores each value it receives.
//!
//! Recording a metric only updates atomics in a sharded registry: there is no lock on a kept
//! handle, no allocation for an existing label set, and no call into the Sentry SDK. Only a
//! histogram value recorded with [event attributes](super::event_labels) takes a short lock for
//! its label set. Once per
//! [`FLUSH_INTERVAL`], [`SentryPolicy::flush_if`] turns what was recorded into one batch of
//! [`SentryMetric`]s and gives it to an adapter that sends it:
//!
//! - each counter's increase since it was last sent, for each label set that increased;
//! - each gauge's latest value, for each label set whose value changed since it was last sent, or
//!   that hasn't been sent for [`GAUGE_REFRESH_FLUSHES`] flushes;
//! - at most [`HISTOGRAM_CAP`] of each histogram's values for each label set, chosen uniformly at
//!   random from the values recorded since the previous flush. Each label set only keeps that
//!   sample, so its memory doesn't grow with how often it is recorded.
//!
//! A batch holds at most [`FLUSH_BUDGET`] metrics. When more are due, counters go first, then
//! gauges that changed, then gauges due for a refresh, then histogram values. Counters and gauges
//! that don't fit stay due and take turns in later flushes, so a counter's increase is delayed,
//! not lost. Histogram values that don't fit are dropped.
//!
//! A flush sends nothing while the Sentry transport is still sending what earlier flushes queued.
//! Its counters and gauges stay due, and its histogram values are dropped, so metrics never take
//! more than their share of a slow or stalled transport's queue.
//!
//! The policy keeps at most [`LABEL_SET_CAP`] label sets for each metric name, and doesn't send
//! the values recorded for any others, so a label that keeps taking new values can't grow the
//! policy's memory or volume without bound. Other backends keep every label set.
//!
//! The `zebrad.sentry.metrics.dropped.total` counter counts the due values left out of a full
//! flush or of a flush the transport wasn't ready for, and the values recorded for label sets over
//! the cap. The policy records it through the `metrics` facade, so every backend receives it, and
//! logs a warning when it starts dropping values.
//!
//! [Event attributes](super::event_labels) never create a label set. Histograms keep them beside
//! each value, and send them without their `event.` prefix. Counters and gauges drop them, because
//! the policy keeps their values for the life of the process.

use std::{
    cell::Cell,
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    mem,
    ops::AddAssign,
    sync::{atomic::Ordering, Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use metrics::{
    atomics::AtomicU64, Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName,
    Label, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::registry::{Registry, Storage};

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
/// 15 envelopes of 100 metrics is half of the 30-envelope transport queue. A flush only sends once
/// the transport has sent what earlier flushes queued, so metrics hold at most these 15 places,
/// plus at exit one partial envelope left by the flush before. At least 14 places stay free for
/// events, logs, and sessions, even while the transport sends nothing.
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

/// A [`Recorder`] that keeps every metric for [`SentryPolicy::flush_if`] to send to Sentry.
pub(crate) struct SentryPolicy {
    registry: Registry<Key, PolicyStorage>,

    /// How many label sets the registry keeps for each metric name. Only registering a label set
    /// the registry doesn't have locks this.
    label_sets: Mutex<LabelSets>,

    /// Counts the values recorded for the label sets the registry doesn't keep.
    untracked: Arc<Untracked>,

    /// The unit of each described metric, by name.
    units: Mutex<HashMap<String, Unit>>,

    /// Held for a whole [`flush_if`](Self::flush_if), so that the flush at shutdown waits for a
    /// periodic flush that has taken counter increases but not sent them yet.
    flushing: Mutex<FlushState>,
}

thread_local! {
    /// Whether this thread is in a [`SentryPolicy::flush_if`], so that the panic hook's flush
    /// returns instead of waiting for a flush that panicked on the same thread.
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
    #[cfg(test)]
    pub(crate) fn flush(&self, send: impl FnOnce(Vec<SentryMetric>)) {
        self.flush_if(|| true, send);
    }

    /// Gives `send` the metrics that are due, as one batch of at most [`FLUSH_BUDGET`] metrics, if
    /// `transport_ready` returns true. Otherwise gives `send` an empty batch: counters and gauges
    /// stay due, and histogram values are dropped.
    ///
    /// Calls `transport_ready` once any flush in progress has finished, so that it can wait for the
    /// transport to send that flush. Builds the batch before calling `send`, so `send` can record
    /// metrics, and this can run from the panic hook.
    pub(crate) fn flush_if(
        &self,
        transport_ready: impl FnOnce() -> bool,
        send: impl FnOnce(Vec<SentryMetric>),
    ) {
        if IN_FLUSH.get() {
            return;
        }

        let mut state = self.flushing.lock().unwrap_or_else(PoisonError::into_inner);
        IN_FLUSH.set(true);

        let mut dropped = Dropped {
            label_set_cap: self.untracked.take(),
            ..Dropped::default()
        };
        let batch = if transport_ready() {
            let (batch, left_out) = self.take_batch(&mut state, FLUSH_BUDGET);
            dropped.flush_budget = left_out;
            batch
        } else {
            let (_, left_out) = self.take_batch(&mut state, 0);
            dropped.transport_backlog = left_out;
            Vec::new()
        };
        let capped = mem::take(&mut self.lock_label_sets().newly_capped);

        dropped.record();
        if let Some((dropped, capped)) = state.warning.add(dropped, capped, Instant::now()) {
            tracing::warn!(
                flush_budget = dropped.flush_budget,
                transport_backlog = dropped.transport_backlog,
                label_set_cap = dropped.label_set_cap,
                capped_metrics = ?capped,
                "some metric values weren't sent to Sentry; \
                 zebrad.sentry.metrics.dropped.total counts them by reason",
            );
        }

        send(batch);
        IN_FLUSH.set(false);
    }

    /// Takes the metrics that are due, and returns at most `room` of them, with how many didn't
    /// fit. Only [`flush_if`](Self::flush_if) calls this.
    fn take_batch(&self, state: &mut FlushState, mut room: usize) -> (Vec<SentryMetric>, u64) {
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
            let values = distribution.take_sample(HISTOGRAM_CAP);
            if !values.is_empty() {
                histograms.push((key.clone(), values));
            }
        });

        let units = self.units.lock().unwrap_or_else(PoisonError::into_inner);
        let unit = |key: &Key| units.get(key.name()).copied();
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
    /// Values that were due in a flush that sent nothing, because the transport was still sending
    /// what earlier flushes queued.
    transport_backlog: u64,
    /// Values recorded for label sets over [`LABEL_SET_CAP`].
    label_set_cap: u64,
}

impl Dropped {
    /// Counts these values in the installed recorder.
    fn record(&self) {
        for (reason, values) in [
            ("flush_budget", self.flush_budget),
            ("transport_backlog", self.transport_backlog),
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
        self.transport_backlog = self
            .transport_backlog
            .saturating_add(other.transport_backlog);
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

/// A uniform sample of the values one histogram label set recorded since the previous flush.
///
/// Each sample keeps at most [`HISTOGRAM_CAP`] values, so a histogram's memory and flush work stay
/// the same however often it is recorded.
#[derive(Default)]
struct Distribution {
    /// Values recorded without event attributes.
    values: Sample,
    /// Values recorded with event attributes, beside those attributes.
    ///
    /// Zebra records these about once per block, so a lock here costs little. The values that
    /// some histograms record thousands of times a minute have no event attributes, and don't take
    /// it.
    with_event_labels: Mutex<SampleWithEventLabels>,
}

impl Distribution {
    /// Takes the samples of the interval, and returns at most `cap` of their values, chosen
    /// uniformly at random from every value recorded in the interval, with their event attributes.
    fn take_sample(&self, cap: usize) -> Vec<(f64, Option<Arc<[Label]>>)> {
        let (values, recorded) = self.values.take();
        let SampleWithEventLabels {
            recorded: recorded_with_event_labels,
            values: with_event_labels,
        } = mem::take(
            &mut *self
                .with_event_labels
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );

        let mut values: Vec<_> = values.into_iter().map(|value| (value, None)).collect();
        let mut with_event_labels: Vec<_> = with_event_labels
            .into_iter()
            .map(|(value, event_labels)| (value, Some(event_labels)))
            .collect();
        if values.len() + with_event_labels.len() <= cap {
            values.append(&mut with_event_labels);
            return values;
        }

        // Each sample is uniform over its own values, so drawing from each one in proportion to
        // the values it hasn't drawn yet keeps the result uniform over all of them.
        let mut left = [recorded, recorded_with_event_labels];
        let mut samples = [values, with_event_labels];
        let mut sample = Vec::with_capacity(cap);
        while sample.len() < cap {
            let from = usize::from(random_below(left[0].saturating_add(left[1])) >= left[0]);
            // A value recorded while the previous flush took the sample can leave this sample
            // with fewer values than its count, so draw from the other one when it runs out.
            let from = if samples[from].is_empty() {
                1 - from
            } else {
                from
            };
            if samples[from].is_empty() {
                break;
            }
            left[from] = left[from].saturating_sub(1);
            let len = u64::try_from(samples[from].len()).expect("a sample fits in u64");
            let index =
                usize::try_from(random_below(len)).expect("an index below a sample's len fits");
            sample.push(samples[from].swap_remove(index));
        }
        sample
    }
}

impl HistogramFn for Distribution {
    fn record(&self, value: f64) {
        self.values.record(value);
    }
}

/// A uniform sample of at most [`HISTOGRAM_CAP`] values, kept with [Algorithm R], in atomic slots
/// so that recording a value takes no lock and allocates nothing.
///
/// A value recorded while a flush takes the sample can land in the next interval's sample.
///
/// [Algorithm R]: https://en.wikipedia.org/wiki/Reservoir_sampling#Simple:_Algorithm_R
struct Sample {
    /// How many values were recorded in the interval.
    recorded: AtomicU64,
    /// The sampled values as `f64` bits, or [`EMPTY_SLOT`].
    slots: [AtomicU64; HISTOGRAM_CAP],
}

/// A slot that holds no value. Samples only keep finite values, so NaN never stands for one.
const EMPTY_SLOT: u64 = f64::NAN.to_bits();

impl Default for Sample {
    fn default() -> Self {
        Self {
            recorded: AtomicU64::new(0),
            slots: std::array::from_fn(|_| AtomicU64::new(EMPTY_SLOT)),
        }
    }
}

impl Sample {
    /// Adds `value` to the sample, unless it isn't finite, which Sentry can't store.
    fn record(&self, value: f64) {
        if !value.is_finite() {
            return;
        }

        let index = self.recorded.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = sample_slot(index) {
            self.slots[slot].store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// Takes the sampled values, with how many values were recorded, and starts a new interval.
    fn take(&self) -> (Vec<f64>, u64) {
        let recorded = self.recorded.swap(0, Ordering::Relaxed);
        if recorded == 0 {
            return (Vec::new(), 0);
        }

        let values = self
            .slots
            .iter()
            .map(|slot| f64::from_bits(slot.swap(EMPTY_SLOT, Ordering::Relaxed)))
            .filter(|value| value.is_finite())
            .collect();
        (values, recorded)
    }
}

/// A uniform sample of at most [`HISTOGRAM_CAP`] values recorded with event attributes, beside
/// those attributes, kept like a [`Sample`].
#[derive(Default)]
struct SampleWithEventLabels {
    /// How many values were recorded in the interval.
    recorded: u64,
    values: Vec<(f64, Arc<[Label]>)>,
}

impl SampleWithEventLabels {
    /// Adds `value` and its event attributes to the sample, unless the value isn't finite.
    fn record(&mut self, value: f64, event_labels: &Arc<[Label]>) {
        if !value.is_finite() {
            return;
        }

        let index = self.recorded;
        self.recorded = self.recorded.saturating_add(1);
        match sample_slot(index) {
            Some(slot) if slot < self.values.len() => {
                self.values[slot] = (value, event_labels.clone());
            }
            Some(_) => self.values.push((value, event_labels.clone())),
            None => {}
        }
    }
}

/// Returns the slot of a [`HISTOGRAM_CAP`]-value sample that the value numbered `index` in an
/// interval replaces, if it is sampled: each of the first values takes the next slot, then each
/// later value replaces a random slot with probability `HISTOGRAM_CAP / (index + 1)`.
fn sample_slot(index: u64) -> Option<usize> {
    if let Some(index) = usize::try_from(index)
        .ok()
        .filter(|index| *index < HISTOGRAM_CAP)
    {
        return Some(index);
    }

    usize::try_from(random_below(index.saturating_add(1)))
        .ok()
        .filter(|slot| *slot < HISTOGRAM_CAP)
}

thread_local! {
    /// The state of this thread's [SplitMix64] generator, which only picks sampled values, so it
    /// doesn't need to be cryptographically secure.
    ///
    /// [SplitMix64]: https://prng.di.unimi.it/splitmix64.c
    static RANDOM_STATE: Cell<u64> = Cell::new(BuildHasher::hash_one(&RandomState::new(), 0_u8));
}

/// Returns a random number below `bound`, or zero if `bound` is zero.
fn random_below(bound: u64) -> u64 {
    let state = RANDOM_STATE.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
    RANDOM_STATE.set(state);

    let mut random = state;
    random = (random ^ (random >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    random = (random ^ (random >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    random ^= random >> 31;

    // The high 64 bits of a 64-bit number times `bound` are below `bound`, so they fit in u64.
    // This is biased by at most `bound / 2^64`, which doesn't matter for sampling.
    ((u128::from(random) * u128::from(bound)) >> 64) as u64
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
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(value, &self.event_labels);
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
    fn a_flush_the_transport_isnt_ready_for_sends_nothing_and_counts_what_was_due() {
        let policy = SentryPolicy::default();
        let histogram_values = 3;

        with_local_recorder(&policy, || {
            metrics::counter!("a.count").increment(2);
            metrics::gauge!("a.peers").set(8.0);
            let histogram = metrics::histogram!("a.duration_seconds");
            for _ in 0..histogram_values {
                histogram.record(1.0);
            }
        });

        let mut sent = vec![];
        with_local_recorder(&policy, || {
            policy.flush_if(|| false, |batch| sent = batch);
        });
        assert!(
            sent.is_empty(),
            "nothing is sent while the transport is behind"
        );

        let next = flush(&policy);
        let value = |name: &str| {
            next.iter()
                .find(|metric| metric.name == name)
                .map(|metric| metric.value)
        };
        assert_eq!(value("a.count"), Some(2.0), "counters keep their increase");
        assert_eq!(value("a.peers"), Some(8.0), "changed gauges stay due");
        assert_eq!(
            value("a.duration_seconds"),
            None,
            "histogram values are dropped"
        );
        assert_eq!(
            dropped(&next, "transport_backlog"),
            count(1 + 1 + histogram_values),
            "every due value is counted",
        );
        assert_eq!(dropped(&next, "flush_budget"), 0.0);
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
            ..Dropped::default()
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
    fn histograms_send_at_most_the_cap_sampled_from_the_interval() {
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
            capped.iter().all(|value| *value < f64::from(recorded)),
            "only recorded values are sent: {capped:?}",
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
    fn histogram_samples_stay_bounded_without_a_flush() {
        let policy = SentryPolicy::default();
        let recorded = 100_000_u32;

        with_local_recorder(&policy, || {
            let histogram = metrics::histogram!("a.duration_seconds");
            for value in 0..recorded {
                histogram.record(f64::from(value));
            }
            for height in 0..recorded / 10 {
                metrics::histogram!(
                    "a.duration_seconds",
                    "event.chain.height" => height.to_string(),
                )
                .record(f64::from(height));
            }
        });

        let distribution = policy
            .registry
            .get_histogram(&Key::from_name("a.duration_seconds"))
            .expect("the label set was recorded");
        let with_event_labels = distribution
            .with_event_labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(with_event_labels.recorded, u64::from(recorded / 10));
        assert_eq!(
            with_event_labels.values.len(),
            HISTOGRAM_CAP,
            "the sample keeps only the cap"
        );
        drop(with_event_labels);
        assert_eq!(
            distribution.values.recorded.load(Ordering::Relaxed),
            u64::from(recorded),
        );

        assert_eq!(flush(&policy).len(), HISTOGRAM_CAP);
        assert!(flush(&policy).is_empty(), "a flush starts a new interval");
    }

    #[test]
    fn histogram_samples_are_uniform_over_the_interval() {
        let policy = SentryPolicy::default();
        let (intervals, recorded) = (200, 1_000_u32);

        let mut sampled = Vec::new();
        for _ in 0..intervals {
            with_local_recorder(&policy, || {
                let histogram = metrics::histogram!("a.duration_seconds");
                for value in 0..recorded {
                    histogram.record(f64::from(value));
                }
            });
            sampled.extend(flush(&policy).into_iter().map(|metric| metric.value));
        }

        // 4,000 values: each tenth of the interval expects 400, with a standard deviation under
        // 19, and the mean expects 499.5, with a standard deviation under 5. The bounds are more
        // than 6 standard deviations wide, so the test doesn't fail by chance.
        assert_eq!(sampled.len(), intervals * HISTOGRAM_CAP);
        let mut tenths = [0_usize; 10];
        for value in &sampled {
            // The values are whole numbers below `recorded`, so the tenth is below 10.
            tenths[(*value / f64::from(recorded / 10)) as usize] += 1;
        }
        assert!(
            tenths.iter().all(|values| (280..=520).contains(values)),
            "each tenth of the interval is sampled about as often: {tenths:?}",
        );
        let mean = sampled.iter().sum::<f64>() / count(sampled.len());
        assert!((470.0..=530.0).contains(&mean), "mean: {mean}");
    }

    #[test]
    fn values_with_and_without_event_attributes_share_one_sample() {
        let policy = SentryPolicy::default();
        let (intervals, recorded) = (50, 100_u32);

        let mut with_event_labels = 0;
        for _ in 0..intervals {
            with_local_recorder(&policy, || {
                let histogram = metrics::histogram!("a.delay_seconds", "kind" => "tip");
                for value in 0..recorded {
                    histogram.record(-f64::from(value) - 1.0);
                    metrics::histogram!(
                        "a.delay_seconds",
                        "kind" => "tip",
                        "event.chain.height" => value.to_string(),
                    )
                    .record(f64::from(value));
                }
            });

            let sent = flush(&policy);
            assert_eq!(sent.len(), HISTOGRAM_CAP, "the cap is for each label set");
            for metric in sent {
                let kind = ("kind".to_owned(), string("tip"));
                if metric.value >= 0.0 {
                    with_event_labels += 1;
                    assert_eq!(
                        metric.attributes,
                        [
                            kind,
                            (
                                "chain.height".to_owned(),
                                // The values are small whole numbers.
                                AttributeValue::Integer(metric.value as u64),
                            ),
                        ],
                        "each value keeps its own event attributes",
                    );
                } else {
                    assert_eq!(metric.attributes, [kind]);
                }
            }
        }

        // Half of 1,000 values, with a standard deviation under 16.
        assert!(
            (400..=600).contains(&with_event_labels),
            "both kinds of values are sampled in proportion: {with_event_labels}",
        );
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
