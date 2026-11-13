//! Checks that the dashboards, alert rules, and example queries in `docker/observability` only
//! query series that Zebra exports.

use std::{collections::BTreeSet, path::Path};

use regex::Regex;
use serde_json::Value;

use super::{workspace_root, Catalog};

/// Series that the observability stack queries but Zebra doesn't export.
const OTHER_SERIES: &[&str] = &[
    // Prometheus's own scrape health series.
    "up",
];

/// Prefixes of series that the observability stack queries but Zebra doesn't export.
const OTHER_SERIES_PREFIXES: &[&str] = &[
    // Span metrics that Jaeger derives from Zebra's traces.
    "traces_spanmetrics_",
];

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

/// Every series queried by the observability stack is exported by Zebra or listed as another
/// component's series.
#[test]
fn queries_use_exported_series() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let exported: BTreeSet<String> = catalog
        .0
        .iter()
        .flat_map(|(name, entry)| entry.prometheus(name).1)
        .collect();

    let root = workspace_root().join("docker/observability");
    let mut queries = Vec::new();

    for path in files_with_extension(&root.join("grafana/dashboards"), "json") {
        let dashboard: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("dashboards are readable"))
                .expect("dashboards are valid JSON");
        let mut expressions = Vec::new();
        dashboard_queries(&dashboard, &mut expressions);
        queries.extend(expressions.into_iter().map(|expr| (path.clone(), expr)));
    }

    for path in files_with_extension(&root.join("prometheus/rules"), "yml") {
        let rules = std::fs::read_to_string(&path).expect("rules are readable");
        let expressions =
            rule_expressions(&rules).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        queries.extend(expressions.into_iter().map(|expr| (path.clone(), expr)));
    }

    let readme_query = Regex::new(r"query\?query=([^'\s]+)").expect("valid regex");
    let readme = root.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).expect("the README is readable");
    queries.extend(
        readme_query
            .captures_iter(&readme_text)
            .map(|captures| (readme.clone(), captures[1].to_owned())),
    );

    assert!(!queries.is_empty(), "the observability stack has queries");

    let mut errors = BTreeSet::new();
    for (path, query) in &queries {
        for name in series_names(query) {
            let known = exported.contains(&name)
                || OTHER_SERIES.contains(&name.as_str())
                || OTHER_SERIES_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix));
            if !known {
                errors.insert(format!(
                    "{}: {name} isn't a series Zebra exports. The catalog in \
                     docker/observability/metrics-catalog.json lists every exported series.",
                    path.strip_prefix(workspace_root())
                        .unwrap_or(path)
                        .display(),
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

/// Returns the files in `dir` with `extension`.
fn files_with_extension(dir: &Path, extension: &str) -> Vec<std::path::PathBuf> {
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

/// Adds the PromQL expressions of the panels and template variables in `value` to `queries`.
fn dashboard_queries(value: &Value, queries: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("expr" | "query", Value::String(query)) => queries.push(query.clone()),
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

/// Returns the series names that the PromQL expression `query` selects.
///
/// Also accepts Grafana's `label_values(series, label)` template queries.
fn series_names(query: &str) -> BTreeSet<String> {
    let label_values = Regex::new(r"label_values\(\s*([^,)]+)\s*,[^)]*\)").expect("valid regex");
    let strings = Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#).expect("valid regex");
    let matchers = Regex::new(r"\{[^}]*\}").expect("valid regex");
    let ranges = Regex::new(r"\[[^\]]*\]").expect("valid regex");
    let variables = Regex::new(r"\$\{[^}]*\}|\$\w+").expect("valid regex");
    let groupings = Regex::new(r"\b(?:by|without|on|ignoring|group_left|group_right)\s*\([^)]*\)")
        .expect("valid regex");
    let tokens =
        Regex::new(r"\d+(?:\.\d+)?(?:[eE][+-]?\d+)?[a-zA-Z]*|([a-zA-Z_:][a-zA-Z0-9_:]*)(\s*\()?")
            .expect("valid regex");

    let query = label_values.replace_all(query, "$1");
    let query = strings.replace_all(&query, "\"\"");
    let query = matchers.replace_all(&query, "");
    let query = ranges.replace_all(&query, "");
    let query = variables.replace_all(&query, "");
    let query = groupings.replace_all(&query, "");

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

    for rules in [quoted, single_quoted, block, folded, multi_line, empty] {
        let error = rule_expressions(rules).expect_err("unreadable expr values are rejected");
        assert!(error.starts_with("line 3:"), "{error}");
    }
}
