//! Checks that every metric recorded with a literal name, and every literal label key, is in the
//! catalog.

use std::path::Path;

use regex::Regex;

use super::{workspace_root, Catalog, Kind};

/// A `counter!`, `gauge!`, or `histogram!` call found in the workspace sources.
#[derive(Debug)]
struct CallSite {
    location: String,
    kind: Kind,
    /// The metric name, if the call uses a string literal.
    name: Option<String>,
    /// The label keys written as string literals.
    label_keys: Vec<String>,
}

/// Every literal metric name and label key at a call site is in the catalog, with the same kind.
#[test]
fn call_sites_record_cataloged_metrics() {
    let _init_guard = zebra_test::init();

    let catalog = Catalog::build();
    let mut call_sites = Vec::new();
    collect_call_sites(&workspace_root(), &mut call_sites);

    assert!(
        call_sites.iter().any(|call| call.name.is_some()),
        "the scan should find the workspace's metric call sites",
    );

    let mut errors = Vec::new();
    for call in &call_sites {
        let Some(name) = &call.name else {
            for key in &call.label_keys {
                if !catalog
                    .0
                    .values()
                    .any(|entry| entry.labels.contains(&key.as_str()))
                {
                    errors.push(format!(
                        "{}: label key {key:?} isn't in any LABELS table",
                        call.location
                    ));
                }
            }
            continue;
        };

        let Some(entry) = catalog.get(name) else {
            errors.push(format!(
                "{}: {name} isn't described: add it to the describe() function of the crate's \
                 metrics module",
                call.location
            ));
            continue;
        };

        if entry.kind != call.kind {
            errors.push(format!(
                "{}: {name} is recorded as a {} but described as a {}",
                call.location, call.kind, entry.kind
            ));
        }

        for key in &call.label_keys {
            if !entry.labels.contains(&key.as_str()) {
                errors.push(format!(
                    "{}: label key {key:?} of {name} isn't in the crate's LABELS table",
                    call.location
                ));
            }
        }
    }

    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// Adds the metric call sites in the non-test Rust sources under `dir` to `call_sites`.
fn collect_call_sites(dir: &Path, call_sites: &mut Vec<CallSite>) {
    let entries = std::fs::read_dir(dir).expect("workspace directories are readable");

    for entry in entries {
        let path = entry.expect("workspace directories are readable").path();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        if path.is_dir() {
            let skipped =
                file_name.starts_with('.') || matches!(file_name, "target" | "tests" | "benches");
            if !skipped {
                collect_call_sites(&path, call_sites);
            }
        } else if file_name.ends_with(".rs") && file_name != "tests.rs" {
            let source = std::fs::read_to_string(&path).expect("source files are readable");
            let location = path
                .strip_prefix(workspace_root())
                .unwrap_or(&path)
                .display()
                .to_string();
            call_sites.extend(parse_call_sites(&location, &source));
        }
    }
}

/// Returns the metric call sites in `source`, outside comments and `#[cfg(test)]` modules.
fn parse_call_sites(location: &str, source: &str) -> Vec<CallSite> {
    let source = strip_tests_and_comments(source);
    let macro_call = Regex::new(r"\b(counter|gauge|histogram)!\s*\(").expect("valid regex");
    let label_key = Regex::new(r#""((?:[^"\\]|\\.)*)"\s*=>"#).expect("valid regex");

    macro_call
        .captures_iter(&source)
        .map(|captures| {
            let whole = captures.get(0).expect("a match has a whole capture");
            let kind = match &captures[1] {
                "counter" => Kind::Counter,
                "gauge" => Kind::Gauge,
                _ => Kind::Histogram,
            };
            let args = macro_args(&source[whole.end()..]);
            let line = source[..whole.start()].matches('\n').count() + 1;

            CallSite {
                location: format!("{location}:{line}"),
                kind,
                name: string_literal(args.trim_start()),
                label_keys: label_key
                    .captures_iter(args)
                    .map(|key| key[1].to_owned())
                    .collect(),
            }
        })
        .collect()
}

/// Returns `source` with `//` comment lines blanked, and with each top-level `#[cfg(test)]`
/// inline module removed. Rustfmt puts the closing brace of a top-level module alone on a line.
///
/// Removed lines are kept as empty lines, so line numbers stay correct.
fn strip_tests_and_comments(source: &str) -> String {
    let mut stripped = Vec::new();
    let mut lines = source.lines().peekable();

    while let Some(line) = lines.next() {
        if line == "#[cfg(test)]"
            && lines
                .peek()
                .is_some_and(|next| next.starts_with("mod ") && next.ends_with('{'))
        {
            stripped.push("");
            for line in lines.by_ref() {
                stripped.push("");
                if line == "}" {
                    break;
                }
            }
        } else if line.trim_start().starts_with("//") {
            stripped.push("");
        } else {
            stripped.push(line);
        }
    }

    stripped.join("\n")
}

/// Returns the arguments of the macro call that starts just after its opening parenthesis.
fn macro_args(rest: &str) -> &str {
    let mut depth = 1;
    let mut in_string = false;
    let mut escaped = false;

    for (i, c) in rest.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..i];
                }
            }
            _ => {}
        }
    }

    rest
}

/// Returns the contents of the string literal at the start of `text`, if there is one.
fn string_literal(text: &str) -> Option<String> {
    let rest = text.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

#[test]
fn parses_literal_and_constant_call_sites() {
    let _init_guard = zebra_test::init();

    let source = r#"
fn record() {
    // metrics::counter!("commented.out").increment(1);
    metrics::counter!("a.count", "kind" => label(), "network" => "mainnet").increment(1);
    metrics::gauge!(NAME, LABEL_KEY => "value").set(1.0);
    metrics::histogram!(
        "a.duration_seconds",
        "result" => if ok { "success" } else { "failure" },
    )
    .record(1.0);
    metrics::describe_counter!("not.a.call.site", "description");
}

#[cfg(test)]
mod tests {
    fn test() {
        metrics::counter!("test.only").increment(1);
    }
}
"#;

    let call_sites = parse_call_sites("file.rs", source);
    let summary: Vec<_> = call_sites
        .iter()
        .map(|call| {
            (
                call.location.as_str(),
                call.kind,
                call.name.as_deref(),
                call.label_keys.clone(),
            )
        })
        .collect();

    assert_eq!(
        summary,
        [
            (
                "file.rs:4",
                Kind::Counter,
                Some("a.count"),
                vec!["kind".to_owned(), "network".to_owned()],
            ),
            ("file.rs:5", Kind::Gauge, None, vec![]),
            (
                "file.rs:6",
                Kind::Histogram,
                Some("a.duration_seconds"),
                vec!["result".to_owned()],
            ),
        ],
    );
}
