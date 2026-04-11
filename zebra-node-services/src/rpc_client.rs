//! A client for calling Zebra's JSON-RPC methods.
//!
//! Used in the rpc sync scanning functionality and in various tests and tools.

use std::{net::SocketAddr, time::Duration};

use reqwest::Client;

use crate::BoxError;

/// The default timeout for RPC requests.
///
/// This is a safety net to prevent RPC calls from hanging indefinitely
/// when a server is alive but unresponsive.
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(65);

/// An HTTP client for making JSON-RPC requests.
#[derive(Clone, Debug)]
pub struct RpcRequestClient {
    client: Client,
    rpc_address: SocketAddr,
}

impl RpcRequestClient {
    /// Creates new RPCRequestSender
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
                .expect("should be able to build reqwest::Client"),
            rpc_address,
        }
    }

    /// Builds rpc request
    pub async fn call(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> reqwest::Result<reqwest::Response> {
        let method = method.as_ref();
        let params = params.as_ref();

        self.client
            .post(format!("http://{}", &self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":123 }}"#
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

        self.client
            .post(format!("http://{}", &self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":123 }}"#
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

        self.client
            .post(format!("http://{}", &self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":123 }}"#
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
    /// Returns an error if the call or result deserialization fail.
    pub async fn json_result_from_call<T: serde::de::DeserializeOwned>(
        &self,
        method: impl AsRef<str>,
        params: impl AsRef<str>,
    ) -> std::result::Result<T, BoxError> {
        Self::json_result_from_response_text(&self.text_from_call(method, params).await?)
    }

    /// Accepts response text from an RPC call
    /// Returns `Ok` with a deserialized `result` value in the expected type, or an error report.
    fn json_result_from_response_text<T: serde::de::DeserializeOwned>(
        response_text: &str,
    ) -> std::result::Result<T, BoxError> {
        let output: jsonrpsee_types::Response<serde_json::Value> =
            serde_json::from_str(response_text)?;
        match output.payload {
            jsonrpsee_types::ResponsePayload::Success(success) => {
                Ok(serde_json::from_value(success.into_owned())?)
            }
            jsonrpsee_types::ResponsePayload::Error(failure) => Err(failure.to_string().into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Proves that `RpcRequestClient` times out instead of hanging indefinitely
    /// when a server accepts a TCP connection but never sends a response.
    #[tokio::test]
    async fn rpc_client_timeout_on_unresponsive_server() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("should bind to localhost");
        let addr = listener.local_addr().expect("should have a local address");

        // Accept the connection but never respond.
        let _accept_thread = std::thread::spawn(move || {
            let (_stream, _peer_addr) = listener.accept().expect("should accept a connection");
            std::thread::park();
        });

        let short_timeout = Duration::from_secs(2);
        let client = RpcRequestClient::new_with_timeout(addr, short_timeout);

        // Outer timeout is a safety net — should never fire.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            client.text_from_call("getinfo", "[]"),
        )
        .await;

        let inner_result = result
            .expect("outer safety timeout should not fire; client timeout should fire first");

        let err =
            inner_result.expect_err("request to unresponsive server should fail with timeout");
        assert!(err.is_timeout(), "error should be a timeout, got: {err}");
    }
}
