//! Zebra supported RPC methods.

use jsonrpc_core;

use jsonrpc_core::Result;
use jsonrpc_derive::rpc;

#[rpc]
/// RPC method signatures.
pub trait Rpc {
    /// getinfo
    #[rpc(name = "getinfo")]
    fn getinfo(&self) -> Result<GetInfo>;
}

/// RPC method implementations.
pub struct RpcImpl;
impl Rpc for RpcImpl {
    fn getinfo(&self) -> Result<GetInfo> {
        // TODO: dummy output data, fix in the context of #3142
        let info = GetInfo {
            build: "Zebra v1.0.0 ...".into(),
            subversion: "/Zebra:1.0.0-beta.4/".into(),
        };

        Ok(info)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Return structure of a `getinfo` RPC method.
pub struct GetInfo {
    build: String,
    subversion: String,
}
