//! A client for calling Zebra's JSON-RPC methods.
//!
//! Only used in tests and tools.

use std::net::SocketAddr;

use reqwest::Client;

use color_eyre::{eyre::eyre, Result};

/// An HTTP client for making JSON-RPC requests.
#[derive(Clone, Debug)]
pub struct RpcRequestClient {
    client: Client,
    rpc_address: SocketAddr,
}

impl RpcRequestClient {
    /// Creates new RPCRequestSender
    pub fn new(rpc_address: SocketAddr) -> Self {
        Self {
            client: Client::new(),
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
    ) -> Result<T> {
        Self::json_result_from_response_text(&self.text_from_call(method, params).await?)
    }

    /// Accepts response text from an RPC call
    /// Returns `Ok` with a deserialized `result` value in the expected type, or an error report.
    fn json_result_from_response_text<T: serde::de::DeserializeOwned>(
        response_text: &str,
    ) -> Result<T> {
        use jsonrpc_core::Output;

        let output: Output = serde_json::from_str(response_text)?;
        match output {
            Output::Success(success) => Ok(serde_json::from_value(success.result)?),
            Output::Failure(failure) => Err(eyre!("RPC call failed with: {failure:?}")),
        }
    }
}
