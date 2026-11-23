//! Tests for the log formats.

use std::{
    io,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use tracing::{error_span, info, info_span, warn};
use tracing_subscriber::{fmt, layer::SubscriberExt, Registry};

use super::*;
use crate::components::tracing::LogFormat;

/// A writer that keeps everything written to it.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl io::Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs `work` with logs written in `format`, and returns the lines it logged.
fn log_lines(format: LogFormat, work: impl FnOnce()) -> Vec<String> {
    let output = Output::default();
    let writer = output.clone();
    let (event_format, field_format) = EventFormat::new(format);
    let layer = fmt::layer()
        .fmt_fields(field_format)
        .event_format(event_format)
        .with_ansi(false)
        .with_writer(move || writer.clone());

    tracing::subscriber::with_default(Registry::default().with(layer), work);

    let output = output.0.lock().expect("not poisoned");
    String::from_utf8(output.clone())
        .expect("logs are UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Runs `work` with JSON logs, and returns each line parsed as a JSON object.
fn json_lines(work: impl FnOnce()) -> Vec<serde_json::Map<String, Value>> {
    log_lines(LogFormat::Json, work)
        .iter()
        .map(|line| match serde_json::from_str(line) {
            Ok(Value::Object(object)) => object,
            other => panic!("line is not a JSON object: {other:?}\n{line}"),
        })
        .collect()
}

#[test]
fn json_line_has_stable_keys_in_order() {
    let lines = json_lines(|| {
        // The global span, which every line would repeat.
        let _global =
            error_span!(target: "zebrad::application", "", net = "Main", zebrad = "abc").entered();
        let _req = info_span!("msg_as_req", msg = "inv").entered();
        let _peer = tracing::span!(tracing::Level::INFO, "", peer = "203.0.113.7:8233").entered();

        warn!(height = 3_000_000, hash = ?"00ab", "block was rejected");
    });

    let [line] = lines.as_slice() else {
        panic!("expected one line, got {lines:?}");
    };

    let keys: Vec<_> = line.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "timestamp",
            "level",
            "severity",
            "message",
            "target",
            "height",
            "hash",
            "spans",
            "code.file.path",
            "code.line.number",
        ]
    );

    assert!(
        chrono::DateTime::parse_from_rfc3339(line["timestamp"].as_str().expect("a string")).is_ok(),
        "timestamp is RFC 3339: {line:?}",
    );
    assert_eq!(line["level"], "WARN");
    assert_eq!(line["severity"], "WARNING");
    assert_eq!(line["message"], "block was rejected");
    assert_eq!(line["height"], 3_000_000);
    assert_eq!(line["hash"], "\"00ab\"");
    assert_eq!(
        line["spans"],
        serde_json::json!([
            {"name": "msg_as_req", "msg": "inv"},
            {"name": "", "peer": "203.0.113.7:8233"},
        ])
    );
    assert!(line["code.line.number"].is_u64());
}

/// Checks the mapping directly, because release builds compile out DEBUG and TRACE events.
#[test]
fn json_severity_matches_each_level() {
    let levels: Vec<_> = [
        Level::ERROR,
        Level::WARN,
        Level::INFO,
        Level::DEBUG,
        Level::TRACE,
    ]
    .iter()
    .map(|level| (level.as_str(), severity(level)))
    .collect();

    assert_eq!(
        levels,
        [
            ("ERROR", "ERROR"),
            ("WARN", "WARNING"),
            ("INFO", "INFO"),
            ("DEBUG", "DEBUG"),
            ("TRACE", "DEBUG"),
        ]
    );
}

#[test]
fn json_fields_named_like_stable_keys_are_renamed() {
    let lines = json_lines(|| {
        let _span = info_span!("work", name = "inner").entered();
        info!(target = "peer", severity = 3, "renamed");
    });

    let line = &lines[0];
    assert_eq!(line["target"], module_path!());
    assert_eq!(line["field.target"], "peer");
    assert_eq!(line["severity"], "INFO");
    assert_eq!(line["field.severity"], 3);
    assert_eq!(
        line["spans"],
        serde_json::json!([{"name": "work", "field.name": "inner"}])
    );
}

#[test]
fn json_values_are_escaped_and_exact() {
    let lines = json_lines(|| {
        info!(
            quoted = "a \"quote\", a \\ backslash,\na newline, and a \u{1} control",
            large = u64::MAX,
            negative = -(1_i64 << 60),
            infinite = f64::INFINITY,
            fraction = 0.5,
            flag = true,
            "unicode: \u{2603}",
        );
    });

    let line = &lines[0];
    assert_eq!(
        line["quoted"],
        "a \"quote\", a \\ backslash,\na newline, and a \u{1} control"
    );
    assert_eq!(line["large"], u64::MAX.to_string());
    assert_eq!(line["negative"], (-(1_i64 << 60)).to_string());
    assert_eq!(line["infinite"], "inf");
    assert_eq!(line["fraction"], 0.5);
    assert_eq!(line["flag"], true);
    assert_eq!(line["message"], "unicode: \u{2603}");
}

#[test]
fn json_span_fields_recorded_later_are_included() {
    let lines = json_lines(|| {
        let span = info_span!(
            "request",
            method = "getinfo",
            status = tracing::field::Empty
        );
        span.record("status", "ok");
        span.in_scope(|| info!("done"));
    });

    assert_eq!(
        lines[0]["spans"],
        serde_json::json!([{"name": "request", "method": "getinfo", "status": "ok"}])
    );
}

#[test]
fn json_long_values_and_lines_are_truncated() {
    let long = "\u{e9}".repeat(MAX_VALUE_LEN);

    let lines = log_lines(LogFormat::Json, || {
        info!(long = %long, "one long value");
        info!(
            a = %long, b = %long, c = %long, d = %long, e = %long,
            "too many long values",
        );
    });

    for line in &lines {
        assert!(line.len() <= MAX_LINE_LEN, "line is {} bytes", line.len());
        let _: Value = serde_json::from_str(line).expect("truncated lines are still JSON");
    }

    let value: Value = serde_json::from_str(&lines[0]).expect("checked above");
    let truncated = value["long"].as_str().expect("a string");
    assert!(truncated.ends_with(TRUNCATED));
    assert!(truncated.len() <= MAX_VALUE_LEN + TRUNCATED.len());

    let value: Value = serde_json::from_str(&lines[1]).expect("checked above");
    assert_eq!(value["message"], "too many long values");
    assert_eq!(value["truncated"], true);
    assert!(value.get("a").is_none());
}

#[test]
fn text_format_is_unchanged() {
    let lines = log_lines(LogFormat::Text, || {
        let _span = info_span!("request", method = "getinfo").entered();
        info!(height = 1, "done");
    });

    assert!(
        lines[0].ends_with(&format!(
            " INFO request{{method=\"getinfo\"}}: {}: done height=1",
            module_path!()
        )),
        "{}",
        lines[0],
    );
}

#[cfg(feature = "opentelemetry")]
#[test]
fn json_line_has_ids_of_its_own_sampled_trace() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};

    let lines_with = |sampler: Sampler| {
        let provider = SdkTracerProvider::builder().with_sampler(sampler).build();
        let output = Output::default();
        let writer = output.clone();
        let (event_format, field_format) = EventFormat::new(LogFormat::Json);
        let subscriber = Registry::default()
            .with(
                fmt::layer()
                    .fmt_fields(field_format)
                    .event_format(event_format)
                    .with_writer(move || writer.clone()),
            )
            .with(super::super::otel::exporting_layer(provider.tracer("test")));

        tracing::subscriber::with_default(subscriber, || {
            let _req =
                info_span!(target: "zebra_network::peer::connection", "msg_as_req").entered();
            info!("in a request");
            info!(parent: None, "outside any span");

            let unentered = info_span!(target: "zebra_network::peer::connection", "msg_as_req");
            info!(parent: &unentered, "in a request that was never entered");
        });

        let output = output.0.lock().expect("not poisoned");
        String::from_utf8(output.clone())
            .expect("logs are UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("a JSON line"))
            .collect::<Vec<_>>()
    };

    let sampled = lines_with(Sampler::AlwaysOn);
    let [in_request, outside, unentered] = sampled.as_slice() else {
        panic!("expected three lines, got {sampled:?}");
    };
    let trace_id = in_request["trace_id"]
        .as_str()
        .expect("a sampled trace has IDs");
    assert_eq!(trace_id.len(), 32);
    assert_eq!(in_request["span_id"].as_str().map(str::len), Some(16));
    assert!(outside.get("trace_id").is_none(), "{outside}");
    assert!(unentered.get("trace_id").is_none(), "{unentered}");

    let unsampled = lines_with(Sampler::AlwaysOff);
    assert!(
        unsampled.iter().all(|line| line.get("trace_id").is_none()),
        "{unsampled:?}"
    );
}

/// Panics with `message`, and returns the JSON line for the panic.
fn json_panic(message: String) -> String {
    let line = Arc::new(Mutex::new(String::new()));
    let hook_line = line.clone();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        *hook_line.lock().expect("not poisoned") = json_panic_line(panic_info);
    }));
    let _ = std::panic::catch_unwind(|| panic!("{message}"));
    std::panic::set_hook(previous_hook);

    let line = line.lock().expect("not poisoned");
    line.clone()
}

#[test]
fn json_long_panic_fits_on_one_line() {
    let line = json_panic("\n".repeat(4 * MAX_VALUE_LEN));

    assert!(line.len() <= MAX_LINE_LEN, "line is {} bytes", line.len());
    let line: Value = serde_json::from_str(&line).expect("a JSON object");
    assert!(line["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(line["code.line.number"].is_u64());
}

#[test]
fn json_escaped_values_fit_on_one_line() {
    let controls = "\u{1}".repeat(MAX_LINE_LEN);

    let lines = log_lines(
        LogFormat::Json,
        || info!(a = %controls, b = %controls, "{controls}"),
    );

    assert!(
        lines[0].len() <= MAX_LINE_LEN,
        "line is {} bytes",
        lines[0].len()
    );
    let line: Value = serde_json::from_str(&lines[0]).expect("a JSON object");
    assert!(line["message"]
        .as_str()
        .is_some_and(|message| message.ends_with(TRUNCATED)));
}

#[test]
fn json_panic_is_one_line() {
    let line = json_panic("state is \"corrupt\"\nat height 7".to_string());
    assert!(!line.contains('\n'), "{line}");
    assert!(line.len() <= MAX_LINE_LEN, "line is {} bytes", line.len());

    let line: serde_json::Map<String, Value> = serde_json::from_str(&line).expect("a JSON object");
    let keys: Vec<_> = line.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "timestamp",
            "level",
            "severity",
            "message",
            "target",
            "thread",
            "exception.type",
            "exception.message",
            "exception.stacktrace",
            "code.file.path",
            "code.line.number",
        ]
    );
    assert_eq!(line["severity"], "CRITICAL");
    assert_eq!(line["message"], "state is \"corrupt\"\nat height 7");
    assert!(line["exception.stacktrace"]
        .as_str()
        .is_some_and(|stacktrace| !stacktrace.is_empty()));
}
