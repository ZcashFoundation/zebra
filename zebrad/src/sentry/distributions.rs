//! Sends selected histograms to Sentry as distributions, and selected counters and gauges once a
//! minute.

use std::sync::Arc;

use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};

/// The histograms sent to Sentry, all measured in seconds.
///
/// Sentry stores every recorded value, so only histograms recorded a few times per block belong
/// here, not ones recorded per transaction, peer message, or request.
const DISTRIBUTIONS: &[&str] = &[
    "zcash.chain.tip.block_delay_seconds",
    "zebra.consensus.block.phase.duration_seconds",
    "state.block_writer.phase.duration_seconds",
];

/// A [`Recorder`] that sends the [`DISTRIBUTIONS`] histograms and the [`periodic`] counters and
/// gauges to Sentry, with their labels as attributes, and ignores every other metric.
///
/// [`periodic`]: super::periodic
#[derive(Debug)]
pub(crate) struct SentryRecorder;

impl Recorder for SentryRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        super::periodic::counter(key).unwrap_or_else(Counter::noop)
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        super::periodic::gauge(key).unwrap_or_else(Gauge::noop)
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        if DISTRIBUTIONS.contains(&key.name()) {
            Histogram::from_arc(Arc::new(Distribution(key.clone())))
        } else {
            Histogram::noop()
        }
    }
}

/// A histogram that Sentry receives as a distribution.
struct Distribution(Key);

impl HistogramFn for Distribution {
    fn record(&self, value: f64) {
        let mut distribution = sentry::metrics::distribution(self.0.name().to_owned(), value)
            .unit(sentry::protocol::Unit::Second);

        for label in self.0.labels() {
            distribution = distribution.attribute(label.key().to_owned(), label.value().to_owned());
        }

        distribution.capture();
    }
}
