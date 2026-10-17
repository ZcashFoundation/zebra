//! A client for calling Zebra's JSON-RPC methods.
//!
//! Used in the rpc sync scanning functionality and in various tests and tools.

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use jsonrpsee_types::Id;
use reqwest::Client;

use crate::BoxError;

/// The default timeout for RPC requests.
///
/// This is a safety net to prevent RPC calls from hanging indefinitely
/// when a server is alive but unresponsive.
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// A process-wide, monotonically increasing counter for JSON-RPC request ids, so
/// each request carries a distinct id that its response can be correlated against.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Returns the next distinct JSON-RPC request id.
fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// An HTTP client for making JSON-RPC requests.
#[derive(Clone, Debug)]
pub struct RpcRequestClient {
    client: Client,
    rpc_address: SocketAddr,
}

impl RpcRequestClient {
    /// Creates a new RPC request client with the default timeout.
    pub fn new(rpc_address: SocketAddr) -> Self {
        Self::new_with_timeout(rpc_address, RPC_REQUEST_TIMEOUT)
    }

    /// Creates a new RPC request client with a custom request timeout.
    ///
    /// Use [`RpcRequestClient::new()`] for the default timeout.
    pub fn new_with_timeout(rpc_address: SocketAddr, timeout: Duration) -> Self {
        Self {
            client: Client::builder()
                .timeout(timeout)
                .build()
                .expect("reqwest::Client build should not fail when only setting timeout"),
            rpc_address,
        }
    }

    /// Builds rpc request
    pub async fn call(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> reqwest::Result<reqwest::Response> {
        self.call_with_id(method.as_ref(), params.as_ref(), next_request_id())
            .await
    }

    /// Builds an rpc request with the given request `id` and an `application/json`
    /// content type.
    async fn call_with_id(
        &self,
        method: &str,
        params: &str,
        id: u64,
    ) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(format!("http://{}", self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":{id} }}"#
            ))
            .header("Content-Type", "application/json")
            .send()
            .await
    }

    /// Builds rpc request with a variable `content-type`.
    pub async fn call_with_content_type(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
        content_type: String,
    ) -> reqwest::Result<reqwest::Response> {
        let method = method.as_ref();
        let params = params.as_ref();
        let id = next_request_id();

        self.client
            .post(format!("http://{}", self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":{id} }}"#
            ))
            .header("Content-Type", content_type)
            .send()
            .await
    }

    /// Builds rpc request with no content type.
    pub async fn call_with_no_content_type(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> reqwest::Result<reqwest::Response> {
        let method = method.as_ref();
        let params = params.as_ref();
        let id = next_request_id();

        self.client
            .post(format!("http://{}", self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":{id} }}"#
            ))
            .send()
            .await
    }

    /// Builds rpc request and gets text from response
    pub async fn text_from_call(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> reqwest::Result<String> {
        self.call(method, params).await?.text().await
    }

    /// Builds an RPC request, awaits its response, and attempts to deserialize
    /// it to the expected result type.
    ///
    /// Returns Ok with json result from response if successful.
    /// Returns an error if the call fails, the response is missing the `jsonrpc`
    /// version, a successful response's `id` doesn't match the request, or result
    /// deserialization fails.
    pub async fn json_result_from_call<T: serde::de::DeserializeOwned>(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> std::result::Result<T, BoxError> {
        let id = next_request_id();
        let response_text = self
            .call_with_id(method.as_ref(), params.as_ref(), id)
            .await?
            .text()
            .await?;

        Self::json_result_from_response_text(&response_text, Id::Number(id))
    }

    /// Accepts response text from an RPC call and the id of the request it answers.
    ///
    /// Returns `Ok` with a deserialized `result` value in the expected type, or an
    /// error report. Rejects a response whose `jsonrpc` version is absent (a
    /// non-`"2.0"` version already fails to deserialize), and a *successful*
    /// response whose `id` does not match `expected_id`. Error payloads are
    /// returned as-is (a null `id` is valid on some JSON-RPC errors).
    fn json_result_from_response_text<T: serde::de::DeserializeOwned>(
        response_text: &str,
        expected_id: Id<'_>,
    ) -> std::result::Result<T, BoxError> {
        let output: jsonrpsee_types::Response<serde_json::Value> =
            serde_json::from_str(response_text)?;

        // A wrong `jsonrpc` version already fails to deserialize above (the field
        // only accepts "2.0"), so here we only need to reject a missing version.
        if output.jsonrpc.is_none() {
            return Err("JSON-RPC response is missing the \"jsonrpc\" version field".into());
        }

        match output.payload {
            jsonrpsee_types::ResponsePayload::Success(success) => {
                // Correlate the result to its request. The id check is only applied
                // to success responses: JSON-RPC allows a null `id` on errors where
                // the request id could not be determined, so an error is returned
                // as-is rather than masked by an id mismatch.
                if output.id != expected_id {
                    return Err(format!(
                        "JSON-RPC response id {:?} does not match request id {expected_id:?}",
                        output.id
                    )
                    .into());
                }

                Ok(serde_json::from_value(success.into_owned())?)
            }
            jsonrpsee_types::ResponsePayload::Error(failure) => Err(failure.to_string().into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Proves that `RpcRequestClient` times out instead of hanging indefinitely
    /// when a server accepts a TCP connection but never sends a response.
    #[tokio::test]
    async fn rpc_client_timeout_on_unresponsive_server() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("should bind to localhost");
        let addr = listener.local_addr().expect("should have a local address");

        // Hold the accepted stream so the connection stays open until the test ends.
        let held_stream = Arc::new(Mutex::new(None));
        let held_stream_clone = held_stream.clone();

        // Accept the connection but never respond.
        let accept_thread = std::thread::spawn(move || {
            let (stream, _peer_addr) = listener.accept().expect("should accept a connection");
            *held_stream_clone.lock().expect("lock poisoned") = Some(stream);
        });

        let short_timeout = Duration::from_secs(2);
        let client = RpcRequestClient::new_with_timeout(addr, short_timeout);

        // Outer timeout is a safety net — should never fire.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            client.text_from_call("getinfo", "[]"),
        )
        .await;

        let inner_result =
            result.expect("outer safety timeout should not fire; client timeout should fire first");

        let err =
            inner_result.expect_err("request to unresponsive server should fail with timeout");
        assert!(err.is_timeout(), "error should be a timeout, got: {err}");

        drop(held_stream);
        accept_thread
            .join()
            .expect("accept thread should exit cleanly");
    }

    /// Regression test for #10687: the client validates the `jsonrpc` version and
    /// the response `id`, rejecting a mismatched id or a missing version.
    #[test]
    fn validates_version_and_id() {
        // A matching version and id deserializes the result.
        let matching = r#"{"jsonrpc":"2.0","result":42,"id":7}"#;
        let value: i64 = RpcRequestClient::json_result_from_response_text(matching, Id::Number(7))
            .expect("a matching response should deserialize");
        assert_eq!(value, 42);

        // A mismatched id is rejected.
        let mismatched_id = r#"{"jsonrpc":"2.0","result":42,"id":8}"#;
        assert!(
            RpcRequestClient::json_result_from_response_text::<i64>(mismatched_id, Id::Number(7))
                .is_err(),
            "a response with a mismatched id should be rejected",
        );

        // A missing jsonrpc version is rejected.
        let missing_version = r#"{"result":42,"id":7}"#;
        assert!(
            RpcRequestClient::json_result_from_response_text::<i64>(missing_version, Id::Number(7))
                .is_err(),
            "a response missing the jsonrpc version should be rejected",
        );
    }

    /// An error response is returned as an error and surfaces the server's message,
    /// even when its `id` is null (which JSON-RPC permits on some errors) and so
    /// does not match the request id. See #10687.
    #[test]
    fn error_response_is_returned_regardless_of_id() {
        let error =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"method not found"},"id":null}"#;
        let err = RpcRequestClient::json_result_from_response_text::<i64>(error, Id::Number(7))
            .expect_err("an error response should be returned as an error");
        assert!(
            err.to_string().contains("method not found"),
            "the error should surface the server's message, got: {err}",
        );
    }
}
