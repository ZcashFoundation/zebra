//! Checks that every catalog metric reaches every backend, and that one Sentry flush fits in the
//! Sentry SDK's transport queue.

use std::sync::Arc;

use metrics::{Key, Label, Level, Metadata, Recorder};

use crate::components::metrics::{
    describe_all,
    event_labels::EVENT_LABEL_PREFIX,
    install, prometheus_builder,
    sentry_policy::{AttributeValue, MetricType, SentryMetric, SentryPolicy, HISTOGRAM_CAP},
    with_prometheus,
};

use super::{Catalog, Entry, Kind};

/// The most metrics the Sentry SDK queues for sending: 30 envelopes of up to 100 items.
const SENTRY_QUEUE_ITEMS: usize = 30 * 100;

/// How many values each label key takes on a busy node within one Sentry flush interval, based on
/// the values each crate's `LABELS` table documents. Keys that aren't listed take
/// [`OTHER_LABEL_VALUES`].
const LABEL_VALUES: &[(&str, usize)] = &[
    // A node runs on one network, at one network upgrade.
    ("network", 1),
    ("chain.network_upgrade", 1),
    ("min_version", 1),
    ("missing_service", 1),
    ("direction", 2),
    ("address_family", 2),
    ("made_progress", 2),
    ("service", 2),
    ("status", 2),
    ("result", 3),
    ("version", 3),
    ("implementation", 4),
    ("remote_version", 4),
    ("negotiated_version", 4),
    ("bk", 5),
    ("name", 5),
    ("verifier", 5),
    ("error_code", 5),
    ("stage", 6),
    ("level", 7),
    ("reason", 9),
    ("kind", 10),
    ("outcome", 10),
    ("error", 11),
    // RPC methods that a node's clients call within a minute.
    ("method", 20),
    ("command", 30),
    ("cf", 30),
    // State request variants.
    ("type", 40),
];

/// How many values a label key that [`LABEL_VALUES`] doesn't list takes.
const OTHER_LABEL_VALUES: usize = 4;

static METADATA: Metadata<'static> =
    Metadata::new(module_path!(), Level::INFO, Some(module_path!()));

/// Records `value` for the metric `key` of `kind` with `recorder`.
fn record(recorder: &dyn Recorder, kind: Kind, key: &Key, value: u32) {
    match kind {
        Kind::Counter => recorder
            .register_counter(key, &METADATA)
            .increment(value.into()),
        Kind::Gauge => recorder
            .register_gauge(key, &METADATA)
            .set(f64::from(value)),
        Kind::Histogram => recorder
            .register_histogram(key, &METADATA)
            .record(f64::from(value)),
    }
}

/// Returns the type Sentry stores a metric of `kind` as.
fn sentry_type(kind: Kind) -> MetricType {
    match kind {
        Kind::Counter => MetricType::Counter,
        Kind::Gauge => MetricType::Gauge,
        Kind::Histogram => MetricType::Distribution,
    }
}

/// Every catalog metric, recorded once through the same recorders as `zebrad`, reaches the
/// Prometheus endpoint and the next Sentry flush.
#[test]
fn every_catalog_metric_reaches_every_backend() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let prometheus = prometheus_builder()
        .expect("the histogram buckets are valid")
        .build_recorder();
    let endpoint = prometheus.handle();
    let sentry = Arc::new(SentryPolicy::default());
    let recorder = with_prometheus(prometheus, Some(sentry.clone()));

    metrics::with_local_recorder(recorder.as_ref(), describe_all);
    for (name, entry) in &catalog.0 {
        let labels: Vec<Label> = entry
            .labels
            .iter()
            .map(|key| Label::new(*key, "1"))
            .collect();
        record(
            recorder.as_ref(),
            entry.kind,
            &Key::from_parts(name.clone(), labels),
            1,
        );
    }

    let rendered = endpoint.render();
    let mut sent = Vec::new();
    sentry.flush(|batch| sent = batch);

    let mut missing = Vec::new();
    for (name, entry) in &catalog.0 {
        let (_, series_names) = entry.prometheus(name);
        for series in series_names {
            let rendered_series = rendered.lines().any(|line| {
                line.strip_prefix(series.as_str())
                    .is_some_and(|rest| rest.starts_with(['{', ' ']))
            });
            if !rendered_series {
                missing.push(format!(
                    "the Prometheus endpoint has no {series} for {name}"
                ));
            }
        }

        let metric_type = sentry_type(entry.kind);
        if !sent
            .iter()
            .any(|metric| metric.name == *name && metric.metric_type == metric_type)
        {
            missing.push(format!("Sentry receives no {metric_type:?} for {name}"));
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));

    let prometheus_event_label = EVENT_LABEL_PREFIX.replace('.', "_");
    assert!(
        !rendered.contains(&prometheus_event_label),
        "Prometheus drops event attributes:\n{rendered}",
    );
    assert!(
        sent.iter()
            .flat_map(|metric| &metric.attributes)
            .all(|(key, _)| !key.starts_with(EVENT_LABEL_PREFIX)),
        "Sentry receives event attributes without their prefix",
    );
}

/// A node that sends metrics only to Sentry sends its build info, like a node with the
/// Prometheus endpoint open.
///
/// This installs the global recorder, so no other test in this process may install one.
#[test]
fn sentry_only_install_sends_the_build_info() {
    let _init_guard = zebra_test::init();

    let sentry: &'static SentryPolicy = Box::leak(Box::default());
    install(sentry).expect("no other test installs a global recorder");

    let mut sent = Vec::new();
    sentry.flush(|batch| sent = batch);

    let build_info = sent
        .iter()
        .find(|metric| metric.name == "zebrad.build.info")
        .unwrap_or_else(|| panic!("Sentry receives the build info: {sent:?}"));
    assert_eq!(build_info.metric_type, MetricType::Counter);
    assert_eq!(build_info.value, 1.0);
    assert!(
        build_info
            .attributes
            .iter()
            .any(|(key, value)| key == "version"
                && *value == AttributeValue::String(env!("CARGO_PKG_VERSION").into())),
        "the build info carries the version: {build_info:?}",
    );
}

/// Returns a representative set of label sets for `entry`, with [`LABEL_VALUES`] values for each
/// label key, and no event attributes.
fn label_sets(entry: &Entry) -> Vec<Vec<Label>> {
    entry
        .labels
        .iter()
        .filter(|key| !key.starts_with(EVENT_LABEL_PREFIX))
        .fold(vec![Vec::new()], |label_sets, key| {
            let values = LABEL_VALUES
                .iter()
                .find(|(listed, _)| listed == key)
                .map_or(OTHER_LABEL_VALUES, |(_, values)| *values);

            label_sets
                .iter()
                .flat_map(|labels| {
                    (0..values).map(move |value| {
                        let mut labels = labels.clone();
                        labels.push(Label::new(*key, value.to_string()));
                        labels
                    })
                })
                .collect()
        })
}

/// One Sentry flush of every catalog metric, with representative label sets, fits in the Sentry
/// SDK's transport queue, even when every histogram is recorded more often than its cap.
#[test]
fn one_sentry_flush_fits_in_the_transport_queue() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let sentry = SentryPolicy::default();
    let records_per_histogram =
        u32::try_from(5 * HISTOGRAM_CAP).expect("the histogram cap is small");

    for (name, entry) in &catalog.0 {
        let event_keys: Vec<&str> = entry
            .labels
            .iter()
            .copied()
            .filter(|key| key.starts_with(EVENT_LABEL_PREFIX))
            .collect();

        for labels in label_sets(entry) {
            let records = match entry.kind {
                Kind::Histogram => records_per_histogram,
                Kind::Counter | Kind::Gauge => 1,
            };

            for value in 0..records {
                // Each record has new event attribute values, like a block height.
                let mut labels = labels.clone();
                labels.extend(
                    event_keys
                        .iter()
                        .map(|key| Label::new(*key, value.to_string())),
                );
                record(
                    &sentry,
                    entry.kind,
                    &Key::from_parts(name.clone(), labels),
                    value + 1,
                );
            }
        }
    }

    let mut sent: Vec<SentryMetric> = Vec::new();
    sentry.flush(|batch| sent = batch);

    assert!(
        sent.len() <= SENTRY_QUEUE_ITEMS,
        "one flush sends {} metrics, more than the {SENTRY_QUEUE_ITEMS} the Sentry SDK queues",
        sent.len(),
    );
    for (name, entry) in &catalog.0 {
        let label_sets = label_sets(entry).len();
        let per_label_set = match entry.kind {
            Kind::Histogram => HISTOGRAM_CAP,
            Kind::Counter | Kind::Gauge => 1,
        };
        let sent_for_name = sent.iter().filter(|metric| metric.name == *name).count();

        assert_eq!(
            sent_for_name,
            label_sets * per_label_set,
            "{name} sends one value per counter or gauge label set, and the cap per histogram \
             label set",
        );
    }
}
