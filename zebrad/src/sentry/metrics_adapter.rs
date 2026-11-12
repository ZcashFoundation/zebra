//! Sends the batches of the [`SentryPolicy`] through the Sentry SDK.

use std::{sync::OnceLock, thread};

use sentry::protocol::{LogAttribute, Unit};

use crate::components::metrics::sentry_policy::{
    AttributeValue, MetricType, SentryMetric, SentryPolicy, FLUSH_INTERVAL,
};

/// The policy that every metric goes through once [`start`] has run.
static POLICY: OnceLock<SentryPolicy> = OnceLock::new();

/// Creates the policy, and a thread that flushes it every [`FLUSH_INTERVAL`].
pub(super) fn start() {
    let policy = POLICY.get_or_init(SentryPolicy::default);

    let flush = thread::Builder::new()
        .name("sentry metrics flush".to_owned())
        .spawn(move || loop {
            thread::sleep(FLUSH_INTERVAL);
            policy.flush(send);
        });
    if let Err(error) = flush {
        warn!(?error, "unable to start Sentry metrics flush");
    }
}

/// Returns the policy to install as a metrics recorder, if [`start`] has run.
pub(super) fn recorder() -> Option<&'static SentryPolicy> {
    POLICY.get()
}

/// Sends what was recorded since the previous flush, if [`start`] has run.
pub(super) fn flush() {
    if let Some(policy) = POLICY.get() {
        policy.flush(send);
    }
}

/// Captures each metric in `batch` on the current Sentry hub.
pub(super) fn send(batch: Vec<SentryMetric>) {
    for metric in batch {
        let attributes = metric
            .attributes
            .into_iter()
            .map(|(key, value)| (key, log_attribute(value)));
        let unit = metric.unit.and_then(sentry_unit);

        match metric.metric_type {
            MetricType::Counter => attributes
                .fold(
                    sentry::metrics::counter(metric.name, metric.value),
                    |counter, (key, value)| counter.attribute(key, value),
                )
                .capture(),
            MetricType::Gauge => {
                let gauge = sentry::metrics::gauge(metric.name, metric.value);
                let gauge = match unit {
                    Some(unit) => gauge.unit(unit),
                    None => gauge,
                };
                attributes
                    .fold(gauge, |gauge, (key, value)| gauge.attribute(key, value))
                    .capture()
            }
            MetricType::Distribution => {
                let distribution = sentry::metrics::distribution(metric.name, metric.value);
                let distribution = match unit {
                    Some(unit) => distribution.unit(unit),
                    None => distribution,
                };
                attributes
                    .fold(distribution, |distribution, (key, value)| {
                        distribution.attribute(key, value)
                    })
                    .capture()
            }
        }
    }
}

fn log_attribute(value: AttributeValue) -> LogAttribute {
    match value {
        AttributeValue::String(value) => value.into(),
        AttributeValue::Integer(value) => value.into(),
    }
}

/// Returns the Sentry unit for `unit`, or `None` for a count or a unit Sentry doesn't have.
fn sentry_unit(unit: metrics::Unit) -> Option<Unit> {
    use metrics::Unit as M;

    Some(match unit {
        M::Seconds => Unit::Second,
        M::Milliseconds => Unit::Millisecond,
        M::Microseconds => Unit::Microsecond,
        M::Nanoseconds => Unit::Nanosecond,
        M::Bytes => Unit::Byte,
        M::Kibibytes => Unit::Kibibyte,
        M::Mebibytes => Unit::Mebibyte,
        M::Gibibytes => Unit::Gibibyte,
        M::Tebibytes => Unit::Tebibyte,
        M::Percent => Unit::Percent,
        _ => return None,
    })
}
