//! Checks that every catalog metric reaches every backend, and that one Sentry flush stays within
//! the Sentry policy's budget.

use std::{collections::HashSet, sync::Arc};

use metrics::{Key, Label, Level, Metadata, Recorder};

use crate::components::metrics::{
    describe_all,
    event_labels::EVENT_LABEL_PREFIX,
    install, prometheus_builder,
    sentry_policy::{
        AttributeValue, MetricType, SentryMetric, SentryPolicy, FLUSH_BUDGET, HISTOGRAM_CAP,
        LABEL_SET_CAP,
    },
    with_prometheus,
};

use super::{Catalog, Entry, Kind};

/// How many values each label key takes on a busy node within one Sentry flush interval, based on
/// the values each crate's `LABELS` table documents. Keys that aren't listed take
/// [`OTHER_LABEL_VALUES`].
const LABEL_VALUES: &[(&str, usize)] = &[
    // A node runs on one network, at one network upgrade.
    ("network", 1),
    ("chain.network_upgrade", 1),
    // One network's default DNS seeders, plus `custom` for every other initial peer.
    ("seed", 6),
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

/// Metrics that nodes recorded with more label sets than [`LABEL_VALUES`] predicts.
const OBSERVED_LABEL_SETS: &[(&str, usize)] = &[
    // A two-node Regtest run exported 51 connection state series.
    ("zebra.net.connection.state", 51),
];

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

/// Returns a representative set of label sets for the metric `name`, with [`LABEL_VALUES`] values
/// for each label key, or the number [`OBSERVED_LABEL_SETS`] lists, and no event attributes.
fn label_sets(name: &str, entry: &Entry) -> Vec<Vec<Label>> {
    let observed = OBSERVED_LABEL_SETS
        .iter()
        .find(|(listed, _)| *listed == name)
        .map(|(_, label_sets)| *label_sets);

    entry
        .labels
        .iter()
        .filter(|key| !key.starts_with(EVENT_LABEL_PREFIX))
        .enumerate()
        .fold(vec![Vec::new()], |label_sets, (index, key)| {
            let values = match observed {
                Some(label_sets) if index == 0 => label_sets,
                Some(_) => 1,
                None => LABEL_VALUES
                    .iter()
                    .find(|(listed, _)| listed == key)
                    .map_or(OTHER_LABEL_VALUES, |(_, values)| *values),
            };

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

/// One Sentry flush on a busy node, which records every label set of every catalog metric within
/// one interval and every histogram more often than its cap, sends every counter and gauge, and
/// values from every histogram label set, within the flush budget.
#[test]
fn a_busy_node_flush_sends_every_counter_and_gauge_within_the_budget() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let sentry = SentryPolicy::default();
    let records_per_histogram =
        u32::try_from(5 * HISTOGRAM_CAP).expect("the histogram cap is small");

    for (name, entry) in &catalog.0 {
        let label_sets = label_sets(name, entry);
        assert!(
            label_sets.len() <= LABEL_SET_CAP,
            "{name} has {} label sets on a busy node, more than the Sentry policy keeps",
            label_sets.len(),
        );

        let event_keys: Vec<&str> = entry
            .labels
            .iter()
            .copied()
            .filter(|key| key.starts_with(EVENT_LABEL_PREFIX))
            .collect();

        for labels in label_sets {
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
        sent.len() <= FLUSH_BUDGET,
        "one flush sends {} metrics, more than its budget of {FLUSH_BUDGET}",
        sent.len(),
    );
    for (name, entry) in &catalog.0 {
        let label_keys: Vec<&str> = entry
            .labels
            .iter()
            .copied()
            .filter(|key| !key.starts_with(EVENT_LABEL_PREFIX))
            .collect();
        let sent_for_name: Vec<&SentryMetric> =
            sent.iter().filter(|metric| metric.name == *name).collect();
        let sent_label_sets: HashSet<Vec<&(String, AttributeValue)>> = sent_for_name
            .iter()
            .map(|metric| {
                metric
                    .attributes
                    .iter()
                    .filter(|(key, _)| label_keys.contains(&key.as_str()))
                    .collect()
            })
            .collect();
        let label_sets = label_sets(name, entry).len();

        assert_eq!(
            sent_label_sets.len(),
            label_sets,
            "{name} sends values for every label set",
        );
        let most = match entry.kind {
            Kind::Histogram => label_sets * HISTOGRAM_CAP,
            Kind::Counter | Kind::Gauge => label_sets,
        };
        assert!(
            sent_for_name.len() <= most,
            "{name} sends one value per counter or gauge label set, and at most the cap per \
             histogram label set",
        );
    }
}

/// One Sentry flush stays within its budget however many label sets each metric has, and counts
/// every value it leaves out through the facade. The Sentry policy keeps at most the label set cap
/// for each metric, while the Prometheus endpoint keeps every label set.
#[test]
fn one_sentry_flush_stays_within_the_budget_for_any_cardinality() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let prometheus = prometheus_builder()
        .expect("the histogram buckets are valid")
        .build_recorder();
    let endpoint = prometheus.handle();
    let sentry = Arc::new(SentryPolicy::default());
    let recorder = with_prometheus(prometheus, Some(sentry.clone()));

    let label_sets = 2 * LABEL_SET_CAP;
    let records_per_histogram =
        u32::try_from(2 * HISTOGRAM_CAP).expect("the histogram cap is small");
    let mut due = 0;
    let mut over_cap = 0;

    metrics::with_local_recorder(recorder.as_ref(), describe_all);
    for (name, entry) in &catalog.0 {
        let records = match entry.kind {
            Kind::Histogram => records_per_histogram,
            Kind::Counter | Kind::Gauge => 1,
        };

        for label_set in 0..label_sets {
            let key = Key::from_parts(
                name.clone(),
                vec![Label::new("label_set", label_set.to_string())],
            );
            // Every gauge is new, so every gauge has changed.
            for value in 0..records {
                record(recorder.as_ref(), entry.kind, &key, value + 1);
            }

            if label_set < LABEL_SET_CAP {
                due += match entry.kind {
                    Kind::Histogram => HISTOGRAM_CAP,
                    Kind::Counter | Kind::Gauge => 1,
                };
            } else {
                over_cap += u64::from(records);
            }
        }
    }

    let mut sent: Vec<SentryMetric> = Vec::new();
    metrics::with_local_recorder(recorder.as_ref(), || {
        sentry.flush(|batch| sent = batch);
    });

    assert_eq!(sent.len(), FLUSH_BUDGET, "a full flush sends its budget");
    assert!(
        sent.iter()
            .all(|metric| metric.metric_type == MetricType::Counter),
        "counters go first when they fill the budget",
    );

    let rendered = endpoint.render();
    let dropped = |reason: &str| -> Option<u64> {
        let series = format!("zebrad_sentry_metrics_dropped_total{{reason=\"{reason}\"}} ");
        rendered
            .lines()
            .find_map(|line| line.strip_prefix(series.as_str()))
            .and_then(|value| value.parse().ok())
    };
    assert_eq!(
        dropped("flush_budget"),
        u64::try_from(due - FLUSH_BUDGET).ok(),
        "every value left out of the flush is counted, and Prometheus receives the count",
    );
    assert_eq!(
        dropped("label_set_cap"),
        Some(over_cap),
        "every value recorded for a label set over the cap is counted",
    );

    for (name, entry) in &catalog.0 {
        if entry.kind == Kind::Histogram {
            continue;
        }
        let (_, series) = entry.prometheus(name);
        let label_set_series = format!("{}{{label_set=", series[0]);
        let exported = rendered
            .lines()
            .filter(|line| line.starts_with(&label_set_series))
            .count();
        assert_eq!(
            exported, label_sets,
            "Prometheus exports every label set of {name}"
        );
    }
}
