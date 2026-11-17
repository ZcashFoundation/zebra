//! Checks the metric catalog against the checked-in catalog file, the code that records metrics,
//! and the dashboards and alert rules that query them.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};
use serde_json::{json, Value};

use super::{catalog, describe_all, HISTOGRAM_BUCKETS};

mod call_sites;
#[cfg(feature = "prometheus")]
mod exporter;
mod queries;

/// The checked-in catalog, relative to the workspace root.
const CATALOG_FILE: &str = "docker/observability/metrics-catalog.json";

/// Set this environment variable to rewrite [`CATALOG_FILE`] instead of checking it.
const UPDATE_CATALOG_ENV: &str = "ZEBRA_UPDATE_METRICS_CATALOG";

/// The kind of a metric, from the `describe_*!` macro that described it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        })
    }
}

/// One described metric.
#[derive(Debug)]
struct Entry {
    kind: Kind,
    unit: Option<Unit>,
    description: String,
    labels: Vec<&'static str>,
}

impl Entry {
    /// Returns the Prometheus metric type and the series names the exporter renders this metric
    /// as, following `metrics-exporter-prometheus` with unit suffixes turned off.
    fn prometheus(&self, name: &str) -> (&'static str, Vec<String>) {
        let bucketed = bucket_bounds(name).is_some();
        let name = prometheus_name(name);
        match self.kind {
            Kind::Counter => ("counter", vec![name]),
            Kind::Gauge => ("gauge", vec![name]),
            Kind::Histogram if bucketed => (
                "histogram",
                vec![
                    format!("{name}_bucket"),
                    format!("{name}_sum"),
                    format!("{name}_count"),
                ],
            ),
            Kind::Histogram => (
                "summary",
                vec![name.clone(), format!("{name}_sum"), format!("{name}_count")],
            ),
        }
    }
}

/// The metric catalog: every metric that Zebra describes, by name.
#[derive(Debug)]
struct Catalog(BTreeMap<String, Entry>);

impl Catalog {
    /// Builds the catalog from every crate's descriptions and label tables.
    fn build() -> Self {
        let recorder = DescriptionRecorder::default();
        metrics::with_local_recorder(&recorder, describe_all);

        let mut entries = BTreeMap::new();
        for (kind, name, unit, description) in recorder
            .descriptions
            .into_inner()
            .expect("the descriptions lock is only held to push, which can't panic")
        {
            let previous = entries.insert(
                name.clone(),
                Entry {
                    kind,
                    unit,
                    description,
                    labels: Vec::new(),
                },
            );
            assert!(previous.is_none(), "{name} is described more than once");
        }

        for (name, keys) in all_labels() {
            let entry = entries
                .get_mut(*name)
                .unwrap_or_else(|| panic!("a LABELS table lists {name}, which no crate describes"));
            assert!(
                entry.labels.is_empty(),
                "{name} is listed in more than one LABELS table, or twice in one",
            );
            entry.labels = keys.to_vec();
        }

        for (name, _) in HISTOGRAM_BUCKETS {
            let kind = entries.get(*name).map(|entry| entry.kind);
            assert_eq!(
                kind,
                Some(Kind::Histogram),
                "HISTOGRAM_BUCKETS lists {name}, which isn't a described histogram",
            );
        }

        Self(entries)
    }

    fn get(&self, name: &str) -> Option<&Entry> {
        self.0.get(name)
    }

    /// Renders the catalog as the JSON in [`CATALOG_FILE`].
    fn to_json(&self) -> String {
        let metrics: Vec<Value> = self
            .0
            .iter()
            .map(|(name, entry)| {
                let (prometheus_type, series) = entry.prometheus(name);
                let mut prometheus = json!({ "type": prometheus_type });
                if let Some(buckets) = bucket_bounds(name) {
                    prometheus["buckets"] = json!(buckets);
                }
                prometheus["series"] = json!(series);

                json!({
                    "name": name,
                    "kind": entry.kind.to_string(),
                    "unit": entry.unit.map(|unit| unit.as_str()),
                    "description": entry.description,
                    "labels": entry.labels,
                    "prometheus": prometheus,
                })
            })
            .collect();

        let mut json = serde_json::to_string_pretty(&json!({ "metrics": metrics }))
            .expect("catalog values are always serializable");
        json.push('\n');
        json
    }
}

/// Returns the Prometheus buckets configured for the histogram `name`, if any.
fn bucket_bounds(name: &str) -> Option<&'static [f64]> {
    HISTOGRAM_BUCKETS
        .iter()
        .find(|(bucketed, _)| *bucketed == name)
        .map(|(_, buckets)| *buckets)
}

/// Returns the name `metrics-exporter-prometheus` exports `name` under.
fn prometheus_name(name: &str) -> String {
    name.chars()
        .enumerate()
        .map(|(i, c)| {
            let valid =
                c.is_ascii_alphabetic() || c == '_' || c == ':' || (i > 0 && c.is_ascii_digit());
            if valid {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Returns every crate's `LABELS` table entries.
fn all_labels() -> impl Iterator<Item = &'static (&'static str, &'static [&'static str])> {
    [
        zebra_consensus::metrics::LABELS,
        zebra_network::metrics::LABELS,
        zebra_state::metrics::LABELS,
        zebra_rpc::metrics::LABELS,
        catalog::LABELS,
    ]
    .into_iter()
    .flatten()
}

/// Returns the workspace root directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("zebrad is a workspace member, so its manifest directory has a parent")
        .to_path_buf()
}

/// A recorder that keeps every description and ignores recorded values.
///
/// `metrics-util`'s `DebuggingRecorder` can't be used, because its snapshots leave out metrics
/// that are described but never recorded.
#[derive(Default)]
struct DescriptionRecorder {
    descriptions: Mutex<Vec<(Kind, String, Option<Unit>, String)>>,
}

impl DescriptionRecorder {
    fn describe(&self, kind: Kind, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.descriptions
            .lock()
            .expect("the descriptions lock is only held to push, which can't panic")
            .push((
                kind,
                key.as_str().to_owned(),
                unit,
                description.into_owned(),
            ));
    }
}

impl Recorder for DescriptionRecorder {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(Kind::Counter, key, unit, description);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(Kind::Gauge, key, unit, description);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(Kind::Histogram, key, unit, description);
    }

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

/// The checked-in catalog matches the descriptions and label tables in the code.
#[test]
fn catalog_file_is_current() {
    let _init_guard = zebra_test::init();

    let path = workspace_root().join(CATALOG_FILE);
    let generated = Catalog::build().to_json();

    if std::env::var_os(UPDATE_CATALOG_ENV).is_some() {
        std::fs::write(&path, generated).expect("the catalog file is writable");
        return;
    }

    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == generated,
        "{CATALOG_FILE} is out of date. Regenerate it with:\n\
         {UPDATE_CATALOG_ENV}=1 cargo nextest run -p zebrad catalog_file_is_current",
    );
}

/// Every described metric has a unit and a description.
#[test]
fn every_metric_has_a_unit_and_description() {
    let _init_guard = zebra_test::init();

    for (name, entry) in &Catalog::build().0 {
        assert!(entry.unit.is_some(), "{name} is described without a unit");
        assert!(
            !entry.description.trim().is_empty(),
            "{name} is described without a description",
        );
    }
}
