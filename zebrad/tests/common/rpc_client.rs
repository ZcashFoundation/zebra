//! A client for calling Zebra's Json-RPC methods

use std::net::SocketAddr;

use reqwest::Client;

#[cfg(feature = "getblocktemplate-rpcs")]
use color_eyre::{eyre::eyre, Result};

/// An http client for making Json-RPC requests
#[derive(Clone, Debug)]
pub struct RPCRequestClient {
    client: Client,
    rpc_address: SocketAddr,
}

impl RPCRequestClient {
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
        method: &'static str,
        params: impl Into<String>,
    ) -> reqwest::Result<reqwest::Response> {
        let params = params.into();
        self.client
            .post(format!("http://{}", &self.rpc_address))
            .body(format!(
                r#"{{"jsonrpc": "2.0", "method": "{method}", "params": {params}, "id":123 }}"#
            ))
            .header("Content-Type", "application/json")
            .send()
            .await
    }

    /// Builds rpc request and gets text from response
    pub async fn text_from_call(
        &self,
        method: &'static str,
        params: impl Into<String>,
    ) -> reqwest::Result<String> {
        self.call(method, params).await?.text().await
    }

    /// Builds an RPC request, awaits its response, and attempts to deserialize
    /// it to the expected result type.
    ///
    /// Returns Ok with json result from response if successful.
    /// Returns an error if the call or result deserialization fail.
    #[cfg(feature = "getblocktemplate-rpcs")]
    pub async fn json_result_from_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &'static str,
        params: impl Into<String>,
    ) -> Result<T> {
        Self::json_result_from_response_text(&self.text_from_call(method, params).await?)
    }

    /// Accepts response text from an RPC call
    /// Returns `Ok` with a deserialized `result` value in the expected type, or an error report.
    #[cfg(feature = "getblocktemplate-rpcs")]
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
