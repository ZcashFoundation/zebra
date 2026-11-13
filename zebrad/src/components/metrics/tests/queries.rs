//! Checks that the dashboards, alert rules, and example queries in `docker/observability` only
//! query series that Zebra exports, and only use labels those series have.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use regex::Regex;
use serde_json::Value;

use super::{workspace_root, Catalog};

/// Series that the observability stack queries but Zebra doesn't export, with their labels.
const OTHER_SERIES: &[(&str, &[&str])] = &[
    // Prometheus's own scrape health series.
    ("up", &[]),
    // Span metrics that Jaeger's spanmetrics connector derives from Zebra's traces. The
    // Prometheus exporter in `docker/observability/jaeger/config.yaml` names them from its
    // `traces` namespace, the connector's `span_metrics` namespace, and the `ms` unit of the
    // duration histogram.
    ("traces_span_metrics_calls_total", SPAN_METRIC_LABELS),
    (
        "traces_span_metrics_duration_milliseconds_bucket",
        SPAN_METRIC_LABELS,
    ),
    (
        "traces_span_metrics_duration_milliseconds_sum",
        SPAN_METRIC_LABELS,
    ),
    (
        "traces_span_metrics_duration_milliseconds_count",
        SPAN_METRIC_LABELS,
    ),
];

/// The labels of the span metrics that `docker/observability/jaeger/config.yaml` configures.
///
/// The connector always adds `service_name`, `span_name`, `span_kind`, and `status_code`, and
/// adds `rpc_method` and `rpc_system` from the configured dimensions. The exporter adds the
/// constant `service` label. The `otel.status_code` dimension never becomes a label, because
/// `tracing-opentelemetry` turns that span field into the span status instead of an attribute.
const SPAN_METRIC_LABELS: &[&str] = &[
    "service_name",
    "span_name",
    "span_kind",
    "status_code",
    "rpc_method",
    "rpc_system",
    "service",
];

/// Labels that Prometheus adds to every series: the scrape target labels, the metric name, and
/// the bucket and quantile labels of histograms and summaries.
const PROMETHEUS_LABELS: &[&str] = &["instance", "job", "__name__", "le", "quantile"];

/// PromQL keywords that look like metric names.
const PROMQL_KEYWORDS: &[&str] = &[
    "and",
    "or",
    "unless",
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
    "bool",
    "offset",
    "inf",
    "nan",
];

/// A query in the observability stack.
struct Query {
    /// The file the query is in.
    path: PathBuf,
    /// The PromQL expression, or Grafana `label_values` template query.
    expr: String,
    /// The Grafana legend format of the panel target that runs the query, if any.
    legend: Option<String>,
}

impl Query {
    /// Returns the query's file, relative to the workspace root.
    fn file(&self) -> std::path::Display<'_> {
        self.path
            .strip_prefix(workspace_root())
            .unwrap_or(&self.path)
            .display()
    }
}

/// Every series queried by the observability stack is exported by Zebra or listed as another
/// component's series.
#[test]
fn queries_use_exported_series() {
    let _init_guard = zebra_test::init();

    let known = SeriesLabels::build(&Catalog::build());

    let mut errors = BTreeSet::new();
    for query in observability_queries() {
        for name in series_names(&query.expr) {
            if !known.series.contains_key(&name) {
                errors.insert(format!(
                    "{}: {name} isn't a series Zebra exports. The catalog in \
                     docker/observability/metrics-catalog.json lists every exported series.",
                    query.file(),
                ));
            }
        }
    }

    assert!(
        errors.is_empty(),
        "{}",
        errors.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// Every label used by a query in the observability stack is a label of a series it queries.
#[test]
fn queries_use_series_labels() {
    let _init_guard = zebra_test::init();

    let known = SeriesLabels::build(&Catalog::build());

    let mut errors = BTreeSet::new();
    for query in observability_queries() {
        for label in known.unknown_labels(&query.expr, query.legend.as_deref()) {
            errors.insert(format!(
                "{}: {label} isn't a label of the series that `{}` queries. The catalog in \
                 docker/observability/metrics-catalog.json lists the labels of every exported \
                 series, and OTHER_SERIES lists the labels of other components' series.",
                query.file(),
                query.expr,
            ));
        }
    }

    assert!(
        errors.is_empty(),
        "{}",
        errors.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// Returns the queries of the dashboards, alert rules, and README in `docker/observability`.
fn observability_queries() -> Vec<Query> {
    let root = workspace_root().join("docker/observability");
    let mut queries = Vec::new();

    for path in files_with_extension(&root.join("grafana/dashboards"), "json") {
        let dashboard: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("dashboards are readable"))
                .expect("dashboards are valid JSON");
        let mut expressions = Vec::new();
        dashboard_queries(&dashboard, &mut expressions);
        queries.extend(expressions.into_iter().map(|(expr, legend)| Query {
            path: path.clone(),
            expr,
            legend,
        }));
    }

    for path in files_with_extension(&root.join("prometheus/rules"), "yml") {
        let rules = std::fs::read_to_string(&path).expect("rules are readable");
        let expressions =
            rule_expressions(&rules).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        queries.extend(expressions.into_iter().map(|expr| Query {
            path: path.clone(),
            expr,
            legend: None,
        }));
    }

    let readme_query = Regex::new(r"query\?query=([^'\s]+)").expect("valid regex");
    let readme = root.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).expect("the README is readable");
    queries.extend(
        readme_query
            .captures_iter(&readme_text)
            .map(|captures| Query {
                path: readme.clone(),
                expr: captures[1].to_owned(),
                legend: None,
            }),
    );

    assert!(!queries.is_empty(), "the observability stack has queries");
    queries
}

/// The labels of every series that the observability stack can query.
struct SeriesLabels {
    /// The labels of each series, by series name.
    series: BTreeMap<String, BTreeSet<String>>,
    /// The labels that every series has.
    common: BTreeSet<String>,
}

impl SeriesLabels {
    /// Builds the labels of Zebra's exported series in `catalog`, the other components' series,
    /// and the labels that Prometheus adds to every series it scrapes.
    fn build(catalog: &Catalog) -> Self {
        let mut series = BTreeMap::new();
        for (name, entry) in &catalog.0 {
            let labels: BTreeSet<String> = entry.labels.iter().map(ToString::to_string).collect();
            for name in entry.prometheus(name).1 {
                series.insert(name, labels.clone());
            }
        }
        for (name, labels) in OTHER_SERIES {
            series.insert(
                name.to_string(),
                labels.iter().map(ToString::to_string).collect(),
            );
        }

        let config_path = workspace_root().join("docker/observability/prometheus/prometheus.yaml");
        let config = std::fs::read_to_string(&config_path)
            .expect("the Prometheus configuration is readable");
        let scrape_labels = static_scrape_labels(&config)
            .unwrap_or_else(|error| panic!("{}: {error}", config_path.display()));

        let common = PROMETHEUS_LABELS
            .iter()
            .map(ToString::to_string)
            .chain(scrape_labels)
            .collect();

        Self { series, common }
    }

    /// Returns the labels that `query` and its Grafana `legend` use but the series they apply to
    /// don't have.
    ///
    /// A label matcher applies to the series it selects. Grouping clauses, `label_values`
    /// labels, and legend placeholders apply to every series in the query.
    fn unknown_labels(&self, query: &str, legend: Option<&str>) -> BTreeSet<String> {
        let labels_of = |series: &str| self.series.get(series).into_iter().flatten();
        let query_labels: BTreeSet<&String> = series_names(query)
            .iter()
            .flat_map(|series| labels_of(series))
            .collect();

        label_uses(query, legend)
            .into_iter()
            .filter(|(series, label)| {
                let known = match series.as_deref().and_then(|series| self.series.get(series)) {
                    Some(labels) => labels.contains(label),
                    None => query_labels.contains(label),
                };
                !known && !self.common.contains(label)
            })
            .map(|(_, label)| label)
            .collect()
    }
}

/// Returns the files in `dir` with `extension`.
fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("observability directories are readable")
        .map(|entry| {
            entry
                .expect("observability directories are readable")
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    files.sort();
    files
}

/// Adds the PromQL expressions of the panels and template variables in `value` to `queries`,
/// each with the legend format of its panel target.
fn dashboard_queries(value: &Value, queries: &mut Vec<(String, Option<String>)>) {
    match value {
        Value::Object(object) => {
            let legend = object
                .get("legendFormat")
                .and_then(Value::as_str)
                .map(str::to_owned);
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("expr" | "query", Value::String(query)) => {
                        queries.push((query.clone(), legend.clone()))
                    }
                    _ => dashboard_queries(value, queries),
                }
            }
        }
        Value::Array(values) => values
            .iter()
            .for_each(|value| dashboard_queries(value, queries)),
        _ => {}
    }
}

/// Returns the label names that the `labels` of the `static_configs` in the Prometheus
/// configuration `config` add to the scraped series.
///
/// Like [`rule_expressions`], this only reads `labels` written as a block mapping, and returns an
/// error for any other form instead of skipping labels it can't read.
fn static_scrape_labels(config: &str) -> Result<BTreeSet<String>, String> {
    let labels_key = Regex::new(r"^(\s*(?:-\s+)?)labels:(.*)$").map_err(|e| e.to_string())?;
    let label_key = Regex::new(r"^\s*([a-zA-Z_][a-zA-Z0-9_]*):\s").map_err(|e| e.to_string())?;
    let lines: Vec<&str> = config.lines().collect();

    let mut labels = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(captures) = labels_key.captures(line) else {
            continue;
        };
        let key_column = captures[1].len();
        if !captures[2]
            .split('#')
            .next()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return Err(format!(
                "line {}: write the labels as a block mapping, so the labels they add can be \
                 checked",
                index + 1,
            ));
        }

        for (next_index, next) in lines.iter().enumerate().skip(index + 1) {
            if next.trim().is_empty() || next.trim_start().starts_with('#') {
                continue;
            }
            if next.len() - next.trim_start().len() <= key_column {
                break;
            }
            let label = label_key.captures(next).ok_or_else(|| {
                format!(
                    "line {}: write each label as a plain `name: value` entry on its own line, so \
                     the labels can be checked",
                    next_index + 1,
                )
            })?;
            labels.insert(label[1].to_owned());
        }
    }

    Ok(labels)
}

/// Returns the `expr` values of the Prometheus rules file `rules`.
///
/// The workspace has no YAML parser, so this only reads `expr` values written as plain scalars on
/// one line. It returns an error for any other form, such as a quoted or block scalar, instead of
/// skipping an expression it can't check.
fn rule_expressions(rules: &str) -> Result<Vec<String>, String> {
    let expr_key = Regex::new(r"^(\s*(?:-\s+)?)expr:(\s.*)?$").map_err(|e| e.to_string())?;
    let lines: Vec<&str> = rules.lines().collect();

    let mut expressions = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(captures) = expr_key.captures(line) else {
            // An `expr` key the pattern doesn't match, such as a quoted key or a flow mapping,
            // would otherwise skip the check without failing it.
            if line.split('#').next().unwrap_or_default().contains("expr") {
                return Err(format!(
                    "line {}: write each expr as a plain `expr:` key on its own line, so the \
                     series it queries can be checked",
                    index + 1,
                ));
            }
            continue;
        };
        let key_column = captures[1].len();
        let value = captures.get(2).map_or("", |value| value.as_str());
        let value = value.split(" #").next().unwrap_or_default().trim();

        let continues_on_next_line = lines[index + 1..]
            .iter()
            .find(|next| !next.trim().is_empty() && !next.trim_start().starts_with('#'))
            .is_some_and(|next| next.len() - next.trim_start().len() > key_column);
        let unreadable = value.is_empty()
            || value.starts_with(['"', '\'', '|', '>', '&', '*', '!', '{', '['])
            || continues_on_next_line;

        if unreadable {
            return Err(format!(
                "line {}: write the expr value as a plain scalar on one line, so the series it \
                 queries can be checked",
                index + 1,
            ));
        }
        expressions.push(value.to_owned());
    }

    Ok(expressions)
}

/// Matches Grafana's `label_values(series, label)` and `label_values(label)` template queries,
/// capturing the series selector and the label.
fn label_values_regex() -> Regex {
    Regex::new(r"label_values\(\s*(?:(.*?)\s*,\s*)?([a-zA-Z_][a-zA-Z0-9_]*)\s*\)")
        .expect("valid regex")
}

/// Matches quoted PromQL strings.
fn strings_regex() -> Regex {
    Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#).expect("valid regex")
}

/// Matches Grafana template variables.
fn variables_regex() -> Regex {
    Regex::new(r"\$\{[^}]*\}|\$\w+").expect("valid regex")
}

/// Matches PromQL grouping and vector matching clauses, capturing their label lists.
fn groupings_regex() -> Regex {
    Regex::new(r"\b(?:by|without|on|ignoring|group_left|group_right)\s*\(([^)]*)\)")
        .expect("valid regex")
}

/// Returns the series names that the PromQL expression `query` selects.
///
/// Also accepts Grafana's `label_values(series, label)` template queries.
fn series_names(query: &str) -> BTreeSet<String> {
    let matchers = Regex::new(r"\{[^}]*\}").expect("valid regex");
    let ranges = Regex::new(r"\[[^\]]*\]").expect("valid regex");
    let tokens =
        Regex::new(r"\d+(?:\.\d+)?(?:[eE][+-]?\d+)?[a-zA-Z]*|([a-zA-Z_:][a-zA-Z0-9_:]*)(\s*\()?")
            .expect("valid regex");

    let query = label_values_regex().replace_all(query, "$1");
    let query = strings_regex().replace_all(&query, "\"\"");
    let query = matchers.replace_all(&query, "");
    let query = ranges.replace_all(&query, "");
    let query = variables_regex().replace_all(&query, "");
    let query = groupings_regex().replace_all(&query, "");

    tokens
        .captures_iter(&query)
        .filter_map(|token| {
            let name = token.get(1)?.as_str();
            let is_function = token.get(2).is_some();
            let is_keyword = PROMQL_KEYWORDS.contains(&name.to_lowercase().as_str());
            (!is_function && !is_keyword).then(|| name.to_owned())
        })
        .collect()
}

/// Returns the labels that the PromQL expression `query` and its Grafana `legend` use.
///
/// Each label comes with the series name of the selector it matches, or `None` when it applies to
/// every series in the query: in a grouping or vector matching clause, a `label_values` query, or
/// a `{{label}}` legend placeholder.
fn label_uses(query: &str, legend: Option<&str>) -> Vec<(Option<String>, String)> {
    let selectors = Regex::new(r"([a-zA-Z_:][a-zA-Z0-9_:]*)?\s*\{([^}]*)\}").expect("valid regex");
    let matcher = Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*(?:=~|!~|!=|=)").expect("valid regex");
    let placeholders = Regex::new(r"\{\{\s*([^{}\s]+)\s*\}\}").expect("valid regex");

    let mut uses = Vec::new();

    let label_values = label_values_regex();
    for captures in label_values.captures_iter(query) {
        uses.push((None, captures[2].to_owned()));
    }
    let query = label_values.replace_all(query, "$1");
    let query = strings_regex().replace_all(&query, "\"\"");
    let query = variables_regex().replace_all(&query, "");

    for captures in selectors.captures_iter(&query) {
        let series = captures.get(1).map(|series| series.as_str().to_owned());
        for label in matcher.captures_iter(&captures[2]) {
            uses.push((series.clone(), label[1].to_owned()));
        }
    }

    for captures in groupings_regex().captures_iter(&query) {
        for label in captures[1].split(',').map(str::trim) {
            if !label.is_empty() {
                uses.push((None, label.to_owned()));
            }
        }
    }

    for captures in placeholders.captures_iter(legend.unwrap_or_default()) {
        uses.push((None, captures[1].to_owned()));
    }

    uses
}

/// Returns the labels for the label tests: series with known labels, and Prometheus's labels.
fn test_labels() -> SeriesLabels {
    let series = [
        ("rpc_requests_total", &["method"][..]),
        ("rpc_request_duration_seconds_bucket", &["method"]),
        ("sync_block_error_count", &["kind"]),
        ("zcash_net_peers", &[]),
    ]
    .into_iter()
    .map(|(series, labels)| {
        (
            series.to_owned(),
            labels.iter().map(ToString::to_string).collect(),
        )
    })
    .collect();
    let common = PROMETHEUS_LABELS.iter().map(ToString::to_string).collect();

    SeriesLabels { series, common }
}

#[test]
fn accepts_labels_of_the_queried_series() {
    let _init_guard = zebra_test::init();

    let known = test_labels();
    let unknown = |query, legend| known.unknown_labels(query, legend);

    assert_eq!(
        unknown(
            r#"histogram_quantile(0.99, sum by (le, method) (rate(rpc_request_duration_seconds_bucket{job="$job", method!~"getblocktemplate|z_.*"}[${__rate_interval}])))"#,
            Some("{{method}} on {{ instance }}"),
        ),
        BTreeSet::new(),
    );
    assert_eq!(
        unknown(
            "sum(rate(rpc_requests_total[5m])) / on (method) group_left (kind) sync_block_error_count",
            None,
        ),
        BTreeSet::new(),
    );
    assert_eq!(
        unknown("label_values(zcash_net_peers, job)", None),
        BTreeSet::new(),
    );
    assert_eq!(
        unknown(
            r#"rpc_requests_total{method="by (nope)", job=~"$nope"}"#,
            None
        ),
        BTreeSet::new(),
        "labels inside strings and variables aren't label names",
    );
}

#[test]
fn rejects_unknown_labels_wherever_they_are_used() {
    let _init_guard = zebra_test::init();

    let known = test_labels();
    let nope = BTreeSet::from(["nope".to_owned()]);

    for query in [
        r#"rpc_requests_total{nope="x"}"#,
        r#"rate(rpc_requests_total{job="zebra", nope!~"x"}[5m])"#,
        "sum by (method, nope) (rpc_requests_total)",
        "sum (rpc_requests_total) by (nope)",
        "sum without (nope) (rpc_requests_total)",
        "rpc_requests_total / on (nope) sync_block_error_count",
        "rpc_requests_total / ignoring (nope) sync_block_error_count",
        "rpc_requests_total * on (method) group_left (nope) sync_block_error_count",
        "rpc_requests_total * on (method) group_right (nope) sync_block_error_count",
        "label_values(zcash_net_peers, nope)",
        r#"label_values(rpc_requests_total{job="zebra", method="getinfo"}, nope)"#,
        "label_values(nope)",
    ] {
        assert_eq!(known.unknown_labels(query, None), nope, "{query}");
    }

    for legend in ["{{nope}}", "{{method}} {{ nope }}"] {
        assert_eq!(
            known.unknown_labels("rpc_requests_total", Some(legend)),
            nope,
            "{legend}",
        );
    }

    assert_eq!(
        known.unknown_labels(
            r#"rpc_requests_total{kind="invalid"} / sync_block_error_count"#,
            None
        ),
        BTreeSet::from(["kind".to_owned()]),
        "a label matcher only accepts the labels of the series it selects",
    );
}

#[test]
fn reads_static_scrape_labels_and_rejects_other_forms() {
    let _init_guard = zebra_test::init();

    let config = "scrape_configs:\n  - job_name: zebra\n    static_configs:\n      - targets: [\"zebra:9999\"]\n        labels:\n          # The network.\n          network: mainnet\n\n          region: eu\n      - targets: [\"other:9999\"]\n  - job_name: other\n";
    assert_eq!(
        static_scrape_labels(config),
        Ok(BTreeSet::from(["network".to_owned(), "region".to_owned()])),
    );
    assert_eq!(
        static_scrape_labels("scrape_configs:\n  - job_name: zebra\n"),
        Ok(BTreeSet::new()),
    );

    let flow = "static_configs:\n  - targets: [a]\n    labels: {network: mainnet}\n";
    let quoted = "static_configs:\n  - targets: [a]\n    labels:\n      \"network\": mainnet\n";
    for config in [flow, quoted] {
        let error = static_scrape_labels(config).expect_err("unreadable labels are rejected");
        assert!(
            error.starts_with("line 3:") || error.starts_with("line 4:"),
            "{error}"
        );
    }
}

#[test]
fn extracts_series_names_from_promql() {
    let _init_guard = zebra_test::init();

    let names = |query| series_names(query).into_iter().collect::<Vec<_>>();

    assert_eq!(
        names(
            r#"histogram_quantile(0.99, sum(rate(rpc_request_duration_seconds_bucket{job="$job"}[$__rate_interval])) by (le, method)) > 2"#
        ),
        ["rpc_request_duration_seconds_bucket"],
    );
    assert_eq!(
        names("sum(rate(a_total[5m])) / (sum(rate(b_count[5m])) + 1e-3) > 0.5 and on(instance) c"),
        ["a_total", "b_count", "c"],
    );
    assert_eq!(
        names("label_values(zcash_net_peers, job)"),
        ["zcash_net_peers"]
    );
    assert_eq!(
        names("sum without (le) (increase(x_bucket[1h] offset 1d))"),
        ["x_bucket"]
    );
}

#[test]
fn reads_plain_rule_expressions_and_rejects_other_forms() {
    let _init_guard = zebra_test::init();

    let plain = "rules:\n  - alert: A\n    expr: rate(a_total[5m]) > 0 # comment\n    for: 5m\n  - expr: b\n";
    assert_eq!(
        rule_expressions(plain),
        Ok(vec!["rate(a_total[5m]) > 0".to_owned(), "b".to_owned()]),
    );

    let quoted = "rules:\n  - alert: A\n    expr: \"not_a_zebra_series > 0\"\n";
    let single_quoted = "rules:\n  - alert: A\n    expr: 'not_a_zebra_series > 0'\n";
    let block = "rules:\n  - alert: A\n    expr: |\n      not_a_zebra_series > 0\n";
    let folded = "rules:\n  - alert: A\n    expr: >-\n      not_a_zebra_series > 0\n";
    let multi_line =
        "rules:\n  - alert: A\n    expr: rate(a_total[5m])\n      > not_a_zebra_series\n";
    let empty = "rules:\n  - alert: A\n    expr:\n      not_a_zebra_series > 0\n";

    let quoted_key = "rules:\n  - alert: A\n    \"expr\": not_a_zebra_series > 0\n";
    let flow_mapping = "rules:\n  - alert: A\n  - {alert: B, expr: not_a_zebra_series > 0}\n";
    for rules in [quoted_key, flow_mapping] {
        let error = rule_expressions(rules).expect_err("unmatched expr keys are rejected");
        assert!(error.starts_with("line 3:"), "{error}");
    }

    for rules in [quoted, single_quoted, block, folded, multi_line, empty] {
        let error = rule_expressions(rules).expect_err("unreadable expr values are rejected");
        assert!(error.starts_with("line 3:"), "{error}");
    }
}
