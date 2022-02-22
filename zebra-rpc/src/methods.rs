//! Zebra supported RPC methods.

use jsonrpc_core;

use jsonrpc_core::Result;
use jsonrpc_derive::rpc;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
    /// getinfo
    #[rpc(name = "getinfo")]
    fn getinfo(&self) -> Result<GetInfo>;

    /// getblockchaininfo
    #[rpc(name = "getblockchaininfo")]
    fn getblockchaininfo(&self) -> Result<GetBlockChainInfo>;
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

    fn getblockchaininfo(&self) -> Result<GetBlockChainInfo> {
        // TODO: dummy output data, fix in the context of #3143
        let response = GetBlockChainInfo {
            chain: "main".to_string(),
        };

        Ok(response)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Return structure of a `getinfo` RPC method.
pub struct GetInfo {
    build: String,
    subversion: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getblockchaininfo` RPC request.
pub struct GetBlockChainInfo {
    chain: String,
    // TODO: add other fields used by lightwalletd (#3143)
}
