//! Tests for RPC metric method labels.

use std::sync::Arc;

use jsonrpsee::RpcModule;

use super::RpcMetricsMiddleware;

#[test]
fn method_labels_use_registered_names_or_unknown() {
    let mut rpc = RpcModule::new(());
    rpc.register_method("getinfo", |_, _, _| "ok")
        .expect("getinfo is the first method registered in this module");
    let middleware = RpcMetricsMiddleware::new((), Arc::new(rpc.method_names().collect()));

    assert_eq!(middleware.method_label("getinfo"), "getinfo");
    assert_eq!(middleware.method_label("unregistered_method"), "unknown");
}
