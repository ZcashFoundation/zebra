//! Sends the batches of the [`SentryPolicy`] through the Sentry SDK.

use std::{
    io,
    sync::{Condvar, Mutex, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant},
};

use sentry::protocol::{LogAttribute, Unit};

use crate::components::metrics::sentry_policy::{
    AttributeValue, MetricType, SentryMetric, SentryPolicy, ENVELOPE_METRICS, FLUSH_INTERVAL,
};

/// The policy that every metric goes through once [`start`] has created it.
static POLICY: OnceLock<SentryPolicy> = OnceLock::new();

/// How long the flush thread waits between the envelopes of one flush.
///
/// The SDK's batcher queues an envelope as soon as it holds [`ENVELOPE_METRICS`] metrics, so
/// capturing a whole flush at once would queue all of its envelopes together. Waiting lets a
/// responsive transport send each envelope before the next one is queued, so a flush takes about
/// one place in the transport queue at a time, and a full flush takes about 15 seconds.
const ENVELOPE_INTERVAL: Duration = Duration::from_secs(1);

/// Spaces out the envelopes of periodic flushes until Zebra shuts down.
static PACE: Pace = Pace::new(ENVELOPE_INTERVAL);

/// Returns the policy, creating it and a thread that flushes it every [`FLUSH_INTERVAL`] on the
/// first call. Returns `None` and logs an error if that thread can't start.
///
/// Must be called after the tracing subscriber is installed, so that error reaches the logs.
pub(super) fn start() -> Option<&'static SentryPolicy> {
    start_with(&POLICY, || {
        thread::Builder::new()
            .name("sentry metrics flush".to_owned())
            .spawn(|| {
                let mut sending = Duration::ZERO;
                loop {
                    thread::sleep(FLUSH_INTERVAL.saturating_sub(sending));
                    let started = Instant::now();
                    if let Some(policy) = POLICY.get() {
                        policy.flush(|batch| send_paced(batch, &PACE, send));
                    }
                    sending = started.elapsed();
                }
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

/// Sends what is due at once, if [`start`] has created the policy, and makes a periodic flush in
/// progress send the rest of its batch at once. Zebra calls this when it shuts down or panics.
pub(super) fn flush_at_exit() {
    PACE.stop();

    if let Some(policy) = POLICY.get() {
        policy.flush(send);
    }
}

/// Calls `send` with `batch` in envelope-sized parts, waiting on `pace` between parts.
fn send_paced(batch: Vec<SentryMetric>, pace: &Pace, mut send: impl FnMut(Vec<SentryMetric>)) {
    let mut metrics = batch.into_iter();
    let mut envelope: Vec<_> = metrics.by_ref().take(ENVELOPE_METRICS).collect();

    while !envelope.is_empty() {
        send(envelope);
        envelope = metrics.by_ref().take(ENVELOPE_METRICS).collect();
        if !envelope.is_empty() {
            pace.wait();
        }
    }
}

/// Spaces out the envelopes of a flush, until [`stop`](Self::stop) is called.
struct Pace {
    interval: Duration,
    stopped: Mutex<bool>,
    stop: Condvar,
}

impl Pace {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            stopped: Mutex::new(false),
            stop: Condvar::new(),
        }
    }

    /// Waits for the interval, or until [`stop`](Self::stop) is called.
    fn wait(&self) {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self
            .stop
            .wait_timeout_while(stopped, self.interval, |stopped| !*stopped);
    }

    /// Ends any wait in progress, and makes later waits return at once.
    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.stop.notify_all();
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
    use std::{
        io,
        sync::{mpsc, Arc, OnceLock},
        thread,
        time::Duration,
    };

    use sentry::{
        protocol::{EnvelopeItem, ItemContainer},
        test::with_captured_envelopes_options,
        ClientOptions,
    };

    use crate::components::metrics::sentry_policy::{
        MetricType, SentryMetric, ENVELOPE_METRICS, FLUSH_BUDGET,
    };

    use super::{send, send_paced, start_with, Pace};

    fn metrics(count: usize) -> Vec<SentryMetric> {
        (0..count)
            .map(|i| SentryMetric {
                metric_type: MetricType::Counter,
                name: format!("a{i}.count"),
                value: 1.0,
                unit: None,
                attributes: Vec::new(),
            })
            .collect()
    }

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

    /// A paced flush queues one Sentry SDK envelope for each part it sends.
    #[test]
    fn a_full_flush_is_sent_in_sdk_envelopes() {
        let pace = Pace::new(Duration::ZERO);

        let envelopes = with_captured_envelopes_options(
            || send_paced(metrics(FLUSH_BUDGET), &pace, send),
            ClientOptions::default(),
        );
        let metrics_per_envelope: Vec<usize> = envelopes
            .iter()
            .flat_map(|envelope| envelope.items())
            .filter_map(|item| match item {
                EnvelopeItem::ItemContainer(ItemContainer::Metrics(metrics)) => Some(metrics.len()),
                _ => None,
            })
            .collect();

        assert_eq!(
            metrics_per_envelope,
            vec![ENVELOPE_METRICS; FLUSH_BUDGET / ENVELOPE_METRICS],
        );
    }

    #[test]
    fn paced_flushes_wait_between_envelopes_until_stopped() {
        let pace = Arc::new(Pace::new(Duration::from_secs(60 * 60)));
        let (parts_tx, parts_rx) = mpsc::channel();

        let sending = thread::spawn({
            let pace = pace.clone();
            move || {
                send_paced(metrics(2 * ENVELOPE_METRICS + 1), &pace, |part| {
                    parts_tx
                        .send(part.len())
                        .expect("the test thread is waiting");
                })
            }
        });

        assert_eq!(parts_rx.recv(), Ok(ENVELOPE_METRICS));
        assert!(
            parts_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the next part waits",
        );

        pace.stop();
        assert_eq!(parts_rx.recv(), Ok(ENVELOPE_METRICS));
        assert_eq!(parts_rx.recv(), Ok(1), "a stopped pace doesn't wait");
        sending.join().expect("the sending thread doesn't panic");
    }
}
