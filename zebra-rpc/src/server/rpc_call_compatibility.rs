//! Compatibility fixes for JSON-RPC remote procedure calls.
//!
//! These fixes are applied at the JSON-RPC call level,
//! after the RPC request is parsed and split into calls.

use jsonrpsee::server::middleware::rpc::layer::ResponseFuture;
use jsonrpsee::server::middleware::rpc::RpcService;
use jsonrpsee::server::middleware::rpc::RpcServiceT;
use jsonrpsee::MethodResponse;
use jsonrpsee_types::ErrorObject;

use crate::constants::INVALID_PARAMETERS_ERROR_CODE;

/// JSON-RPC [`Middleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to JSON-RPC calls:
///
/// ## Make RPC framework response codes match `zcashd`
///
/// [`jsonrpc_core`] returns specific error codes while parsing requests:
/// <https://docs.rs/jsonrpsee-types/latest/jsonrpsee_types/error/enum.ErrorCode.html>
///
/// But these codes are different from `zcashd`, and some RPC clients rely on the exact code.
/// Specifically, the [`INVALID_PARAMETERS_ERROR_CODE`] is different:
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
                    let new_error_code = INVALID_PARAMETERS_ERROR_CODE.code();
                    tracing::debug!(
                        "Replacing RPC error: {original_error_code} with {new_error_code}"
                    );
                    let json: serde_json::Value =
                        serde_json::from_str(response.into_parts().0.as_str())
                            .expect("response string should be valid json");
                    let id = json["id"]
                        .as_str()
                        .expect("response json should have an id")
                        .to_string();

                    return MethodResponse::error(
                        jsonrpsee_types::Id::Str(id.into()),
                        ErrorObject::borrowed(new_error_code, "Invalid params", None),
                    );
                }
            }
            response
        }))
    }
}
