//! Sends selected histograms to Sentry as distributions.

use std::sync::Arc;

use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use sentry::metrics::DistributionMetric;
use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
};

/// The histograms sent to Sentry, all measured in seconds.
///
/// Sentry stores every recorded value, so only histograms recorded a few times per block belong
/// here, not ones recorded per transaction, peer message, or request. The block phase histograms
/// are only recorded by builds of the `next` branch.
///
/// [`send_tip_block_delay`] sends the tip block delay instead, with its block height.
const DISTRIBUTIONS: &[&str] = &[
    "zebra.consensus.block.phase.duration_seconds",
    "state.block_writer.phase.duration_seconds",
];

/// Sends a tip block delay to Sentry with the block's height, so that nodes can be compared block
/// by block.
///
/// The height isn't a label on the histogram, because Prometheus would keep a series per height.
pub(crate) fn send_tip_block_delay(height: Height, seconds: f64) {
    tip_block_delay(super::NETWORK.get(), height, seconds).capture();
}

/// Returns a tip block delay with the block's height and, once the network is known, its network
/// upgrade.
///
/// The network upgrade comes from the block's own height, because the tip height that other
/// metrics use lags behind a new tip, which would put an activation block in the previous upgrade.
pub(super) fn tip_block_delay(
    network: Option<&Network>,
    height: Height,
    seconds: f64,
) -> DistributionMetric {
    let distribution =
        sentry::metrics::distribution("zcash.chain.tip.block_delay_seconds", seconds)
            .unit(sentry::protocol::Unit::Second)
            .attribute("chain.height", height.0);

    match network {
        Some(network) => distribution.attribute(
            "chain.network_upgrade",
            NetworkUpgrade::current(network, height).to_string(),
        ),
        None => distribution,
    }
}

/// A [`Recorder`] that sends the [`DISTRIBUTIONS`] histograms to Sentry, with their labels as
/// attributes, and ignores every other metric.
#[derive(Debug)]
pub(crate) struct SentryRecorder;

impl Recorder for SentryRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
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
