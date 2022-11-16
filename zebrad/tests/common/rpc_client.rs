//! A client for calling Zebra's Json-RPC methods

use std::net::SocketAddr;

use reqwest::Client;

/// An http client for making Json-RPC requests
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
}
