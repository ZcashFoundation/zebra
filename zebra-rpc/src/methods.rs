//! Zebra supported RPC methods.

use jsonrpc_core::{self, Result};
use jsonrpc_derive::rpc;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
    /// getinfo
    ///
    /// TODO: explain what the method does
    ///       link to the zcashd RPC reference
    ///       list the arguments and fields that lightwalletd uses
    ///       note any other lightwalletd changes
    #[rpc(name = "getinfo")]
    fn get_info(&self) -> Result<GetInfo>;

    /// getblockchaininfo
    ///
    /// TODO: explain what the method does
    ///       link to the zcashd RPC reference
    ///       list the arguments and fields that lightwalletd uses
    ///       note any other lightwalletd changes
    #[rpc(name = "getblockchaininfo")]
    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo>;
}

/// RPC method implementations.
pub struct RpcImpl;
impl Rpc for RpcImpl {
    fn get_info(&self) -> Result<GetInfo> {
        // TODO: dummy output data, fix in the context of #3142
        let response = GetInfo {
            build: "Zebra v1.0.0 ...".into(),
            subversion: "/Zebra:1.0.0-beta.4/".into(),
        };

        Ok(response)
    }

    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo> {
        // TODO: dummy output data, fix in the context of #3143
        let response = GetBlockChainInfo {
            chain: "main".to_string(),
        };

        Ok(response)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getinfo` RPC request.
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
