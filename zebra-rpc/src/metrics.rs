//! Descriptions of the metrics this crate records.
//!
//! Applications that install a [`metrics`] recorder call [`describe`] right after installing it, so
//! that exporters can document each metric, for example in Prometheus `# HELP` lines.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric this crate records.
///
/// - `method`: a JSON-RPC method name the server registers, or `unknown`; or an indexer gRPC
///   method (`ChainTipChange`, `NonFinalizedStateChange`, `MempoolChange`, or `GetBlock`).
/// - `status`: `success` or `error`.
/// - `error_code`: the JSON-RPC error code, such as `-8`, or `unknown`.
/// - `outcome`: a `submitblock` result, `accepted`, `duplicate`, `duplicate-inconclusive`,
///   `inconclusive`, or `rejected`.
pub const LABELS: &[(&str, &[&str])] = &[
    ("rpc.requests.total", &["method", "status"]),
    ("rpc.request.duration_seconds", &["method"]),
    ("rpc.errors.total", &["method", "error_code"]),
    ("rpc.submitblock.outcomes.total", &["outcome"]),
    ("indexer.grpc.request.duration_seconds", &["method"]),
    ("indexer.grpc.streams.active", &["method"]),
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
    describe_gauge!(
        "rpc.connections.active",
        Unit::Count,
        "JSON-RPC HTTP requests currently holding a server connection permit."
    );
    describe_counter!(
        "rpc.connections.rejected.total",
        Unit::Count,
        "JSON-RPC HTTP requests rejected with HTTP 429 because all connection permits were in use."
    );
    describe_histogram!(
        "rpc.request.size_bytes",
        Unit::Bytes,
        "HTTP request body size of accepted JSON-RPC requests, in bytes."
    );
    describe_histogram!(
        "rpc.response.size_bytes",
        Unit::Bytes,
        "HTTP response body size of completed JSON-RPC responses, in bytes."
    );
    describe_histogram!(
        "rpc.batch.size",
        Unit::Count,
        "JSON-RPC calls (including notifications) in each non-empty batch request."
    );
    describe_histogram!(
        "rpc.getblocktemplate.build.duration_seconds",
        Unit::Seconds,
        "Time taken to select transactions and build a block template, including the coinbase."
    );
    describe_histogram!(
        "rpc.getblocktemplate.first_template.duration_seconds",
        Unit::Seconds,
        "Time from a new best chain tip until the first block template on it was served."
    );
    describe_counter!(
        "rpc.submitblock.outcomes.total",
        Unit::Count,
        "Blocks submitted with submitblock, by their zcashd-compatible result."
    );
    describe_counter!(
        "rpc.errors.total",
        Unit::Count,
        "JSON-RPC requests that returned an error, by method and error code."
    );
    describe_histogram!(
        "indexer.grpc.request.duration_seconds",
        Unit::Seconds,
        "Time taken to handle an indexer gRPC unary call or open a server stream, by method."
    );
    describe_gauge!(
        "indexer.grpc.streams.active",
        Unit::Count,
        "Open indexer gRPC server streams, by method."
    );
}

#[cfg(test)]
pub(crate) mod tests;
