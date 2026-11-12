//! Descriptions of the metrics this crate records.
//!
//! Applications that install a [`metrics`] recorder call [`describe`] right after installing it, so
//! that exporters can document each metric, for example in Prometheus `# HELP` lines.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric this crate records.
///
/// - `method`: a JSON-RPC method name the server registers, or `unknown`.
/// - `status`: `success` or `error`.
/// - `error_code`: the JSON-RPC error code, such as `-8`, or `unknown`.
pub const LABELS: &[(&str, &[&str])] = &[
    ("rpc.requests.total", &["method", "status"]),
    ("rpc.request.duration_seconds", &["method"]),
    ("rpc.errors.total", &["method", "error_code"]),
];

/// Describes every metric this crate records to the installed [`metrics`] recorder.
pub fn describe() {
    describe_counter!(
        "rpc.requests.total",
        Unit::Count,
        "JSON-RPC requests handled, by method and status."
    );
    describe_histogram!(
        "rpc.request.duration_seconds",
        Unit::Seconds,
        "Time taken to handle a JSON-RPC request."
    );
    describe_gauge!(
        "rpc.active_requests",
        Unit::Count,
        "JSON-RPC requests currently being handled."
    );
    describe_counter!(
        "rpc.errors.total",
        Unit::Count,
        "JSON-RPC requests that returned an error, by method and error code."
    );
}
