//! Sends the batches of the [`SentryPolicy`] through the Sentry SDK.

use std::{
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak},
    thread,
    time::{Duration, Instant},
};

use sentry::{
    protocol::{LogAttribute, Unit},
    transports::DefaultTransportFactory,
    Transport, TransportFactory, TransportOptions,
};

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

/// How long a flush waits for the Sentry transport to send what is already in its queue, before it
/// sends nothing instead.
///
/// Pacing at [`ENVELOPE_INTERVAL`] assumes the transport sends an envelope a second, so this is
/// enough to finish an envelope in flight, and a queue that takes longer to empty is falling behind.
const TRANSPORT_WAIT: Duration = Duration::from_secs(2);

/// Checks the transport that [`transport_factory`] creates for the Sentry client.
static TRANSPORT: TransportCheck = TransportCheck::new();

/// Returns the policy, creating it, a thread that flushes it every [`FLUSH_INTERVAL`], and a thread
/// that checks the Sentry transport for those flushes, on the first call. Returns `None` and logs an
/// error if those threads can't start.
///
/// Must be called after the tracing subscriber is installed, so that error reaches the logs.
pub(super) fn start() -> Option<&'static SentryPolicy> {
    start_with(&POLICY, || {
        thread::Builder::new()
            .name("sentry transport check".to_owned())
            .spawn(|| TRANSPORT.run())?;

        thread::Builder::new()
            .name("sentry metrics flush".to_owned())
            .spawn(|| {
                let mut sending = Duration::ZERO;
                loop {
                    thread::sleep(FLUSH_INTERVAL.saturating_sub(sending));
                    let started = Instant::now();
                    if let Some(policy) = POLICY.get() {
                        flush_paced(policy, &PACE, transport_ready, send);
                    }
                    sending = started.elapsed();
                }
            })
    })
}

/// Creates the policy in `policy` only if `spawn_flush` starts the threads that flush it.
///
/// Without those threads, the policy would keep every histogram value it records, so Zebra doesn't
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
                "unable to start the Sentry metrics threads: Zebra won't send metrics to Sentry",
            );
            None
        }
    }
}

/// Makes a periodic flush in progress send the rest of its batch at once, then sends what is due
/// at once, if [`start`] has created the policy and the transport has sent what earlier flushes
/// queued. Zebra calls this when it shuts down or panics.
pub(super) fn flush_at_exit() {
    PACE.stop();

    if let Some(policy) = POLICY.get() {
        flush_paced(policy, &PACE, transport_ready, send);
    }
}

/// Returns whether the Sentry transport has sent everything queued before, waiting up to
/// [`TRANSPORT_WAIT`] for it.
fn transport_ready() -> bool {
    TRANSPORT.sent_everything(TRANSPORT_WAIT)
}

/// Flushes `policy` if `transport_ready` returns true, and sends the batch paced by `pace`.
fn flush_paced(
    policy: &SentryPolicy,
    pace: &Pace,
    transport_ready: impl FnOnce() -> bool,
    send: impl FnMut(Vec<SentryMetric>),
) {
    policy.flush_if(transport_ready, |batch| send_paced(batch, pace, send));
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
        let _ = self
            .stop
            .wait_timeout_while(lock(&self.stopped), self.interval, |stopped| !*stopped);
    }

    /// Ends any wait in progress, and makes later waits return at once.
    fn stop(&self) {
        *lock(&self.stopped) = true;
        self.stop.notify_all();
    }
}

/// Returns the factory of the Sentry client's transport: the SDK's default transport, which
/// metrics flushes check before they send.
pub(super) fn transport_factory() -> impl TransportFactory {
    CheckedTransport(&TRANSPORT)
}

/// Creates the SDK's default transport, and gives it to a [`TransportCheck`].
struct CheckedTransport(&'static TransportCheck);

impl TransportFactory for CheckedTransport {
    fn create_transport_with_options(&self, options: TransportOptions) -> Arc<dyn Transport> {
        let transport = DefaultTransportFactory.create_transport_with_options(options);
        self.0.watch(&transport);
        transport
    }
}

/// Checks whether a Sentry transport has sent every envelope queued before the check, one check
/// at a time, on the thread that calls [`run`](Self::run).
///
/// A check flushes the transport itself rather than the Sentry client, because the client holds a
/// lock that its shutdown needs for as long as a flush lasts. The SDK queues a flush behind the
/// envelopes before it, and waits for room in a full queue first, so a check lasts as long as the
/// transport is stalled. Running checks on their own thread keeps flushes taking histogram values
/// meanwhile, and waiting for each check to finish keeps at most one of them in the queue.
struct TransportCheck {
    transport: Mutex<Option<Weak<dyn Transport>>>,
    /// Whether a check has started and not finished.
    pending: Mutex<bool>,
    changed: Condvar,
}

impl TransportCheck {
    const fn new() -> Self {
        Self {
            transport: Mutex::new(None),
            pending: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    /// Checks `transport` from now on.
    fn watch(&self, transport: &Arc<dyn Transport>) {
        *lock(&self.transport) = Some(Arc::downgrade(transport));
    }

    /// Returns whether the transport has sent everything queued before the check in progress, or
    /// before a check this starts if none is in progress, waiting up to `wait` for that check.
    fn sent_everything(&self, wait: Duration) -> bool {
        let mut pending = lock(&self.pending);
        if !*pending {
            *pending = true;
            self.changed.notify_all();
        }

        let (pending, _) = self
            .changed
            .wait_timeout_while(pending, wait, |pending| *pending)
            .unwrap_or_else(PoisonError::into_inner);
        !*pending
    }

    /// Runs the checks that [`sent_everything`](Self::sent_everything) starts, forever.
    fn run(&self) {
        let mut pending = lock(&self.pending);
        loop {
            pending = self
                .changed
                .wait_while(pending, |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner);
            drop(pending);

            let transport = lock(&self.transport).as_ref().and_then(Weak::upgrade);
            if let Some(transport) = transport {
                transport.flush(Duration::MAX);
            }

            pending = lock(&self.pending);
            *pending = false;
            self.changed.notify_all();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Condvar, Mutex, OnceLock,
        },
        thread,
        time::Duration,
    };

    use metrics::with_local_recorder;
    use sentry::{
        protocol::{Envelope, EnvelopeItem, Event, ItemContainer},
        test::with_captured_envelopes_options,
        ClientOptions, Transport, TransportFactory, TransportOptions,
    };

    use crate::components::metrics::sentry_policy::{
        MetricType, SentryMetric, SentryPolicy, ENVELOPE_METRICS, FLUSH_BUDGET,
        TRANSPORT_QUEUE_ENVELOPES,
    };

    use super::{
        flush_paced, lock, send, send_paced, start_with, CheckedTransport, Pace, TransportCheck,
    };

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

    /// A transport whose flushes wait while the test stalls it.
    #[derive(Default)]
    struct StallingTransport {
        stalled: Mutex<bool>,
        resumed: Condvar,
        flushes: AtomicUsize,
    }

    impl StallingTransport {
        fn stall(&self, stalled: bool) {
            *lock(&self.stalled) = stalled;
            self.resumed.notify_all();
        }
    }

    impl Transport for StallingTransport {
        fn send_envelope(&self, _: Envelope) {}

        fn flush(&self, _: Duration) -> bool {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            let _resumed = self
                .resumed
                .wait_while(lock(&self.stalled), |stalled| *stalled);
            true
        }
    }

    /// Returns a check that runs on its own thread for the rest of the test process.
    fn running_check() -> &'static TransportCheck {
        let check: &'static TransportCheck = Box::leak(Box::new(TransportCheck::new()));
        thread::spawn(|| check.run());
        check
    }

    /// Counts one more `a.count`, flushes `policy` if `check` finds that the transport sent
    /// everything within `wait`, and returns the batch.
    fn count_and_flush(
        policy: &SentryPolicy,
        check: &TransportCheck,
        wait: Duration,
    ) -> Vec<SentryMetric> {
        with_local_recorder(policy, || metrics::counter!("a.count").increment(1));

        let mut sent = Vec::new();
        flush_paced(
            policy,
            &Pace::new(Duration::ZERO),
            || check.sent_everything(wait),
            |part| sent.extend(part),
        );
        sent
    }

    /// While the transport hasn't sent what was queued before, flushes send nothing, and one check
    /// at a time waits in its queue. Once it has, the next flush sends what was left due.
    #[test]
    fn flushes_send_nothing_until_the_transport_sends_what_was_queued() {
        let transport = Arc::new(StallingTransport::default());
        let check = running_check();
        check.watch(&(transport.clone() as Arc<dyn Transport>));
        let policy = SentryPolicy::default();

        transport.stall(true);
        for _ in 0..3 {
            assert!(
                count_and_flush(&policy, check, Duration::from_millis(50)).is_empty(),
                "the transport is still sending",
            );
        }
        assert_eq!(
            transport.flushes.load(Ordering::Relaxed),
            1,
            "one check at a time waits in the transport queue",
        );

        transport.stall(false);
        let sent = count_and_flush(&policy, check, Duration::from_secs(60));
        let sent: Vec<_> = sent
            .iter()
            .map(|metric| (metric.name.as_str(), metric.value))
            .collect();
        assert_eq!(
            sent,
            [("a.count", 4.0)],
            "the counter keeps the increase of the flushes that sent nothing",
        );
    }

    /// A flush at exit, after a periodic flush in progress sent the rest of its batch at once,
    /// sends nothing while the transport hasn't sent that batch, so the two flushes queue at most
    /// one budget between them.
    #[test]
    fn a_flush_at_exit_waits_for_the_transport_to_send_the_flush_in_progress() {
        let transport = Arc::new(StallingTransport::default());
        let check = running_check();
        check.watch(&(transport.clone() as Arc<dyn Transport>));
        let policy = Arc::new(SentryPolicy::default());
        let pace = Arc::new(Pace::new(Duration::from_secs(60 * 60)));
        with_local_recorder(&*policy, || {
            for i in 0..2 * FLUSH_BUDGET {
                metrics::counter!(format!("a{i}.count")).increment(1);
            }
        });

        let (parts_tx, parts_rx) = mpsc::channel();
        let send_to_test = |parts_tx: mpsc::Sender<usize>| {
            move |part: Vec<SentryMetric>| {
                parts_tx
                    .send(part.len())
                    .expect("the test thread is waiting");
            }
        };

        let periodic = thread::spawn({
            let (policy, pace, send) =
                (policy.clone(), pace.clone(), send_to_test(parts_tx.clone()));
            move || {
                flush_paced(
                    &policy,
                    &pace,
                    || check.sent_everything(Duration::from_secs(60)),
                    send,
                )
            }
        });
        assert_eq!(
            parts_rx.recv(),
            Ok(ENVELOPE_METRICS),
            "the periodic flush sends its first envelope, then waits",
        );

        transport.stall(true);
        pace.stop();
        flush_paced(
            &policy,
            &pace,
            || check.sent_everything(Duration::from_millis(50)),
            send_to_test(parts_tx),
        );
        periodic.join().expect("the periodic flush doesn't panic");
        transport.stall(false);

        let sent_after_the_first_envelope: usize = parts_rx.iter().sum();
        assert_eq!(
            ENVELOPE_METRICS + sent_after_the_first_envelope,
            FLUSH_BUDGET,
            "the periodic flush sends its budget, and the flush at exit sends nothing while the \
             transport hasn't sent it",
        );
    }

    /// The SDK's HTTP transport sends one envelope at a time, and waits for the server's response
    /// without a timeout. A Sentry server that never responds stalls the transport queue, so
    /// metrics flushes send nothing until the connection fails.
    #[test]
    fn a_sentry_server_that_never_responds_stops_metrics_flushes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the test can listen on localhost");
        let address = listener.local_addr().expect("the listener has an address");
        let mut options =
            ClientOptions::new().transport_channel_capacity(TRANSPORT_QUEUE_ENVELOPES);
        options.dsn = Some(
            format!("http://public@{address}/1")
                .parse()
                .expect("the DSN is valid"),
        );
        let options =
            TransportOptions::try_from_client_options(&options).expect("the options have a DSN");

        let check = running_check();
        let transport = CheckedTransport(check).create_transport_with_options(options);
        let policy = SentryPolicy::default();

        let mut envelope = Envelope::new();
        envelope.add_item(Event::default());
        transport.send_envelope(envelope);
        let (connection, _) = listener
            .accept()
            .expect("the transport connects to send the envelope");

        assert!(
            count_and_flush(&policy, check, Duration::from_millis(200)).is_empty(),
            "the transport is waiting for the server's response",
        );

        drop((connection, listener));
        assert_eq!(
            count_and_flush(&policy, check, Duration::from_secs(60)).len(),
            1,
            "once the connection fails, the transport sends the rest of its queue, and flushes \
             send again",
        );
    }
}
