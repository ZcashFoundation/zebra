//! Compatibility fixes for JSON-RPC remote procedure calls.
//!
//! These fixes are applied at the JSON-RPC call level,
//! after the RPC request is parsed and split into calls.

use std::future::Future;

use futures::future::{Either, FutureExt};
use jsonrpc_core::{
    middleware::Middleware,
    types::{Call, Failure, Output, Response},
    BoxFuture, Metadata, MethodCall, Notification,
};

/// JSON-RPC [`Middleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to JSON-RPC calls:
///
/// ## Make RPC framework response codes match `zcashd`
///
/// [`jsonrpc_core`] returns specific error codes while parsing requests:
/// <https://docs.rs/jsonrpc-core/18.0.0/jsonrpc_core/types/error/enum.ErrorCode.html#variants>
///
/// But these codes are different from `zcashd`, and some RPC clients rely on the exact code.
///
/// ## Read-Only Functionality
///
/// This middleware also logs unrecognized RPC requests.
pub struct FixRpcResponseMiddleware;

impl<M: Metadata> Middleware<M> for FixRpcResponseMiddleware {
    type Future = BoxFuture<Option<Response>>;
    type CallFuture = BoxFuture<Option<Output>>;

    fn on_call<Next, NextFuture>(
        &self,
        call: Call,
        meta: M,
        next: Next,
    ) -> Either<Self::CallFuture, NextFuture>
    where
        Next: Fn(Call, M) -> NextFuture + Send + Sync,
        NextFuture: Future<Output = Option<Output>> + Send + 'static,
    {
        Either::Left(
            next(call.clone(), meta)
                .inspect(|output| Self::log_if_error(output, call))
                .boxed(),
        )
    }
}

impl FixRpcResponseMiddleware {
    /// Obtain a description string for a received request.
    ///
    /// Prints out only the method name and the received parameters.
    fn call_description(call: &Call) -> String {
        match call {
            Call::MethodCall(MethodCall { method, params, .. }) => {
                format!(r#"method = {method:?}, params = {params:?}"#)
            }
            Call::Notification(Notification { method, params, .. }) => {
                format!(r#"notification = {method:?}, params = {params:?}"#)
            }
            Call::Invalid { .. } => "invalid request".to_owned(),
        }
    }

    /// Check RPC output and log any errors.
    //
    // TODO: do we want to ignore ErrorCode::ServerError(_), or log it at debug?
    fn log_if_error(output: &Option<Output>, call: Call) {
        if let Some(Output::Failure(Failure { error, .. })) = output {
            let call_description = Self::call_description(&call);
            tracing::info!("RPC error: {error} in call: {call_description}");
        }
    }
}
