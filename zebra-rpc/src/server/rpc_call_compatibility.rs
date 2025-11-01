//! Compatibility fixes for JSON-RPC remote procedure calls.
//!
//! These fixes are applied at the JSON-RPC call level,
//! after the RPC request is parsed and split into calls.

use jsonrpsee::{
    server::middleware::rpc::{layer::ResponseFuture, RpcService, RpcServiceT},
    MethodResponse,
};
use jsonrpsee_types::ErrorObject;

/// JSON-RPC [`FixRpcResponseMiddleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to JSON-RPC calls:
///
/// ## Make RPC framework response codes match `zcashd`
///
/// [`jsonrpsee_types`] returns specific error codes while parsing requests:
/// <https://docs.rs/jsonrpsee-types/latest/jsonrpsee_types/error/enum.ErrorCode.html>
///
/// But these codes are different from `zcashd`, and some RPC clients rely on the exact code.
/// Specifically, the [`jsonrpsee_types::error::INVALID_PARAMS_CODE`] is different:
/// <https://docs.rs/jsonrpsee-types/latest/jsonrpsee_types/error/constant.INVALID_PARAMS_CODE.html>
pub struct FixRpcResponseMiddleware {
    service: RpcService,
}

impl FixRpcResponseMiddleware {
    /// Create a new `FixRpcResponseMiddleware` with the given `service`.
    pub fn new(service: RpcService) -> Self {
        Self { service }
    }
}

impl<'a> RpcServiceT<'a> for FixRpcResponseMiddleware {
    type Future = ResponseFuture<futures::future::BoxFuture<'a, jsonrpsee::MethodResponse>>;

    fn call(&self, request: jsonrpsee::types::Request<'a>) -> Self::Future {
        let service = self.service.clone();
        ResponseFuture::future(Box::pin(async move {
            let response = service.call(request).await;
            if response.is_error() {
                let original_error_code = response
                    .as_error_code()
                    .expect("response should have an error code");
                if original_error_code == jsonrpsee_types::ErrorCode::InvalidParams.code() {
                    let new_error_code = crate::server::error::LegacyCode::Misc.into();
                    tracing::debug!(
                        "Replacing RPC error: {original_error_code} with {new_error_code}"
                    );
                    let json: serde_json::Value =
                        serde_json::from_str(response.into_parts().0.as_str())
                            .expect("response string should be valid json");
                    let id = match &json["id"] {
                        serde_json::Value::Null => Some(jsonrpsee::types::Id::Null),
                        serde_json::Value::Number(n) => {
                            n.as_u64().map(jsonrpsee::types::Id::Number)
                        }
                        serde_json::Value::String(s) => Some(jsonrpsee::types::Id::Str(s.into())),
                        _ => None,
                    }
                    .expect("response json should have an id");

                    return MethodResponse::error(
                        id,
                        ErrorObject::borrowed(
                            new_error_code,
                            json.get("error")
                                .and_then(|v| v.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("Invalid params"),
                            None,
                        ),
                    );
                }
            }
            response
        }))
    }
}
