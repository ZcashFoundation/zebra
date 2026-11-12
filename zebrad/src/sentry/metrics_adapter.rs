//! Sends the batches of the [`SentryPolicy`] through the Sentry SDK.

use std::{io, sync::OnceLock, thread};

use sentry::protocol::{LogAttribute, Unit};

use crate::components::metrics::sentry_policy::{
    AttributeValue, MetricType, SentryMetric, SentryPolicy, FLUSH_INTERVAL,
};

/// The policy that every metric goes through once [`start`] has created it.
static POLICY: OnceLock<SentryPolicy> = OnceLock::new();

/// Returns the policy, creating it and a thread that flushes it every [`FLUSH_INTERVAL`] on the
/// first call. Returns `None` and logs an error if that thread can't start.
///
/// Must be called after the tracing subscriber is installed, so that error reaches the logs.
pub(super) fn start() -> Option<&'static SentryPolicy> {
    start_with(&POLICY, || {
        thread::Builder::new()
            .name("sentry metrics flush".to_owned())
            .spawn(|| loop {
                thread::sleep(FLUSH_INTERVAL);
                flush();
            })
    })
}

/// Creates the policy in `policy` only if `spawn_flush` starts the thread that flushes it.
///
/// Without that thread, the policy would keep every histogram value it records, so Zebra doesn't
/// record metrics for Sentry at all.
fn start_with<T>(
    policy: &OnceLock<SentryPolicy>,
    spawn_flush: impl FnOnce() -> io::Result<T>,
) -> Option<&SentryPolicy> {
    if let Some(policy) = policy.get() {
        return Some(policy);
    }

    match spawn_flush() {
        Ok(_) => Some(policy.get_or_init(SentryPolicy::default)),
        Err(error) => {
            error!(
                ?error,
                "unable to start the Sentry metrics flush thread: Zebra won't send metrics to Sentry",
            );
            None
        }
    }
}

/// Sends what was recorded since the previous flush, if [`start`] has created the policy.
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

#[cfg(test)]
mod tests {
    use std::{io, sync::OnceLock};

    use super::start_with;

    #[test]
    fn policy_is_created_only_with_a_flush_thread() {
        let _init_guard = zebra_test::init();

        let policy = OnceLock::new();
        let started = start_with(&policy, || {
            Err::<(), _>(io::Error::other("no threads left"))
        });
        assert!(
            started.is_none() && policy.get().is_none(),
            "without a flush thread, the policy would keep every histogram value",
        );

        assert!(start_with(&policy, || Ok(())).is_some());

        let mut spawned_again = false;
        let started_again = start_with(&policy, || {
            spawned_again = true;
            Ok(())
        });
        assert!(started_again.is_some());
        assert!(!spawned_again, "one flush thread flushes the policy");
    }
}
