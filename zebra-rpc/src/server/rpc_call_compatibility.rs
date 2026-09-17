//! Compatibility fixes for JSON-RPC remote procedure calls.
//!
//! These fixes are applied at the JSON-RPC call level,
//! after the RPC request is parsed and split into calls.

use jsonrpsee::{
    server::middleware::rpc::{layer::ResponseFuture, RpcServiceT},
    MethodResponse,
};
use jsonrpsee_types::{ErrorObject, ErrorObjectOwned};

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
#[derive(Clone)]
pub struct FixRpcResponseMiddleware<S> {
    service: S,
}

impl<S> FixRpcResponseMiddleware<S> {
    /// Create a new `FixRpcResponseMiddleware` with the given `service`.
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

impl<'a, S> RpcServiceT<'a> for FixRpcResponseMiddleware<S>
where
    S: RpcServiceT<'a> + Send + Sync + Clone + 'static,
{
    type Future = ResponseFuture<futures::future::BoxFuture<'a, jsonrpsee::MethodResponse>>;

    fn call(&self, request: jsonrpsee::types::Request<'a>) -> Self::Future {
        let service = self.service.clone();

        // The response carries the same id as the request, so keep the id here instead of
        // parsing it back out of the serialized response.
        let id = request.id.clone().into_owned();

        ResponseFuture::future(Box::pin(async move {
            let response = service.call(request).await;

            if response.as_error_code() != Some(jsonrpsee_types::ErrorCode::InvalidParams.code()) {
                return response;
            }

            let new_error_code = crate::server::error::LegacyCode::Misc.into();
            tracing::debug!(
                "Replacing RPC error code {} with {new_error_code}",
                jsonrpsee_types::ErrorCode::InvalidParams.code(),
            );

            // Recover the original error object so only its code changes; its message and data
            // are preserved.
            let error: ErrorObjectOwned =
                serde_json::from_str::<serde_json::Value>(response.as_result())
                    .ok()
                    .and_then(|json| serde_json::from_value(json.get("error")?.clone()).ok())
                    .expect("an InvalidParams response always has a valid error object");

            MethodResponse::error(
                id,
                ErrorObject::owned(new_error_code, error.message(), error.data()),
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use jsonrpsee::{
        server::middleware::rpc::RpcServiceT,
        types::{Id, Request},
        MethodResponse,
    };
    use jsonrpsee_types::{ErrorCode, ErrorObject};

    use crate::server::error::LegacyCode;

    use super::FixRpcResponseMiddleware;

    /// A mock inner service that returns an error with the given code, message and data.
    #[derive(Clone)]
    struct ErrorService {
        code: i32,
        message: &'static str,
        data: Option<serde_json::Value>,
    }

    impl<'a> RpcServiceT<'a> for ErrorService {
        type Future = std::future::Ready<MethodResponse>;

        fn call(&self, request: Request<'a>) -> Self::Future {
            let error = ErrorObject::owned(self.code, self.message, self.data.clone());
            std::future::ready(MethodResponse::error(request.id.into_owned(), error))
        }
    }

    fn request() -> Request<'static> {
        Request::new("getinfo".into(), None, Id::Number(42))
    }

    /// An `InvalidParams` error has its code replaced with the `zcashd`-compatible `Misc` code,
    /// while its id, message and data are preserved.
    #[tokio::test]
    async fn replaces_invalid_params_code_preserving_id_message_and_data() {
        let inner = ErrorService {
            code: ErrorCode::InvalidParams.code(),
            message: "bad params",
            data: Some(serde_json::json!({ "detail": "x" })),
        };

        let response = FixRpcResponseMiddleware::new(inner).call(request()).await;

        assert!(response.is_error());
        assert_eq!(response.as_error_code(), Some(i32::from(LegacyCode::Misc)));

        let json: serde_json::Value =
            serde_json::from_str(response.as_result()).expect("response should be valid json");
        assert_eq!(json["id"], serde_json::json!(42));
        assert_eq!(
            json["error"]["code"],
            serde_json::json!(i32::from(LegacyCode::Misc))
        );
        assert_eq!(json["error"]["message"], "bad params");
        assert_eq!(json["error"]["data"], serde_json::json!({ "detail": "x" }));
    }

    /// Errors with any other code pass through unchanged.
    #[tokio::test]
    async fn leaves_other_error_codes_unchanged() {
        let inner = ErrorService {
            code: ErrorCode::MethodNotFound.code(),
            message: "nope",
            data: None,
        };

        let response = FixRpcResponseMiddleware::new(inner).call(request()).await;

        assert_eq!(
            response.as_error_code(),
            Some(ErrorCode::MethodNotFound.code())
        );
        let json: serde_json::Value =
            serde_json::from_str(response.as_result()).expect("response should be valid json");
        assert_eq!(json["error"]["message"], "nope");
    }
}
