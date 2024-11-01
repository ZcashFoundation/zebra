//! Define JSON-RPC request and response structures

/// The JSON-RPC request
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    id: String,
    method: String,
    params: Vec<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Get the method name from the request
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Get the parameters from the request
    pub fn params(&self) -> &[serde_json::Value] {
        &self.params
    }

    /// Get the request ID
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The JSON-RPC response
#[derive(serde::Serialize)]
pub struct JsonRpcResponse {
    //jsonrpc: String,
    result: serde_json::Value,
    id: String,
    //error: Option<String>,
}

impl JsonRpcResponse {
    ///
    pub fn new(result: serde_json::Value, id: String) -> Self {
        Self { result, id }
    }
}

/// The JSON-RPC error
#[derive(serde::Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}
