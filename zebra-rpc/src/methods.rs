//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `lightwalletd` client implementation.

use futures::{FutureExt, TryFutureExt};
use hex::FromHex;
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    block::SerializedBlock,
    serialization::{SerializationError, ZcashDeserialize},
    transaction::{self, Transaction},
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::{mempool, BoxError};

#[cfg(test)]
mod tests;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
    /// getinfo
    ///
    /// Returns software information from the RPC server running Zebra.
    ///
    /// zcashd reference: <https://zcash.github.io/rpc/getinfo.html>
    ///
    /// Result:
    /// {
    ///      "build": String, // Full application version
    ///      "subversion", String, // Zebra user agent
    /// }
    ///
    /// Note 1: We only expose 2 fields as they are the only ones needed for
    /// lightwalletd: <https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L91-L95>
    ///
    /// Note 2: <https://zcash.github.io/rpc/getinfo.html> is outdated so it does not
    /// show the fields we are exposing. However, this fields are part of the output
    /// as shown in the following zcashd code:
    /// <https://github.com/zcash/zcash/blob/v4.6.0-1/src/rpc/misc.cpp#L86-L87>
    /// Zcash open ticket to add this fields to the docs: <https://github.com/zcash/zcash/issues/5606>
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

    /// sendrawtransaction
    ///
    /// Sends the raw bytes of a signed transaction to the network, if the transaction is valid.
    ///
    /// zcashd reference: <https://zcash.github.io/rpc/sendrawtransaction.html>
    ///
    /// Result: a hexadecimal string of the hash of the sent transaction.
    ///
    /// Note: zcashd provides an extra `allowhighfees` parameter, but we don't yet because
    /// lightwalletd doesn't use it.
    #[rpc(name = "sendrawtransaction")]
    fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
    ) -> BoxFuture<Result<SentTransactionHash>>;

    /// getblock
    ///
    /// Returns requested block by height, encoded as hex.
    ///
    /// zcashd reference: <https://zcash.github.io/rpc/getblock.html>
    ///
    /// Result:
    /// {
    ///      "data": String, // The block encoded as hex
    /// }
    ///
    /// Note 1: We only expose the `data` field as lightwalletd uses the non-verbose
    /// mode for all getblock calls: <https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L232>
    ///
    /// Note 2: `lightwalletd` only requests blocks by height, so we don't support
    /// getting blocks by hash.
    ///
    /// Note 3: The `verbosity` parameter is ignored but required in the call.
    #[rpc(name = "getblock")]
    fn get_block(&self, height: String, verbosity: u8) -> BoxFuture<Result<GetBlock>>;
}

/// RPC method implementations.
pub struct RpcImpl<Mempool, State>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>,
    State: Service<
        zebra_state::Request,
        Response = zebra_state::Response,
        Error = zebra_state::BoxError,
    >,
{
    /// Zebra's application version.
    app_version: String,
    /// A handle to the mempool service.
    mempool: Buffer<Mempool, mempool::Request>,
    /// A handle to the state service.
    state: Buffer<State, zebra_state::Request>,
}

impl<Mempool, State> RpcImpl<Mempool, State>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>,
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + 'static,
    State::Future: Send,
{
    /// Create a new instance of the RPC handler.
    pub fn new(
        app_version: String,
        mempool: Buffer<Mempool, mempool::Request>,
        state: Buffer<State, zebra_state::Request>,
    ) -> Self {
        RpcImpl {
            app_version,
            mempool,
            state,
        }
    }
}

impl<Mempool, State> Rpc for RpcImpl<Mempool, State>
where
    Mempool:
        tower::Service<mempool::Request, Response = mempool::Response, Error = BoxError> + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + 'static,
    State::Future: Send,
{
    fn get_info(&self) -> Result<GetInfo> {
        let response = GetInfo {
            build: self.app_version.clone(),
            subversion: USER_AGENT.into(),
        };

        Ok(response)
    }

    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo> {
        // TODO: dummy output data, fix in the context of #3143
        let response = GetBlockChainInfo {
            chain: "TODO: main".to_string(),
        };

        Ok(response)
    }

    fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
    ) -> BoxFuture<Result<SentTransactionHash>> {
        let mempool = self.mempool.clone();

        async move {
            let raw_transaction_bytes = Vec::from_hex(raw_transaction_hex).map_err(|_| {
                Error::invalid_params("raw transaction is not specified as a hex string")
            })?;
            let raw_transaction = Transaction::zcash_deserialize(&*raw_transaction_bytes)
                .map_err(|_| Error::invalid_params("raw transaction is structurally invalid"))?;

            let transaction_hash = raw_transaction.hash();

            let transaction_parameter = mempool::Gossip::Tx(raw_transaction.into());
            let request = mempool::Request::Queue(vec![transaction_parameter]);

            let response = mempool.oneshot(request).await.map_err(|error| Error {
                code: ErrorCode::ServerError(0),
                message: error.to_string(),
                data: None,
            })?;

            let queue_results = match response {
                mempool::Response::Queued(results) => results,
                _ => unreachable!("incorrect response variant from mempool service"),
            };

            assert_eq!(
                queue_results.len(),
                1,
                "mempool service returned more results than expected"
            );

            match &queue_results[0] {
                Ok(()) => Ok(SentTransactionHash(transaction_hash)),
                Err(error) => Err(Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                }),
            }
        }
        .boxed()
    }

    fn get_block(&self, height: String, _verbosity: u8) -> BoxFuture<Result<GetBlock>> {
        let mut state = self.state.clone();

        async move {
            let height = height.parse().map_err(|error: SerializationError| Error {
                code: ErrorCode::ServerError(0),
                message: error.to_string(),
                data: None,
            })?;

            let request = zebra_state::Request::Block(zebra_state::HashOrHeight::Height(height));
            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            match response {
                zebra_state::Response::Block(Some(block)) => Ok(GetBlock { data: block.into() }),
                zebra_state::Response::Block(None) => Err(Error {
                    code: ErrorCode::ServerError(0),
                    message: "Block not found".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a block request"),
            }
        }
        .boxed()
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

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
/// Response to a `sendrawtransaction` RPC request.
///
/// A JSON string with the transaction hash in hexadecimal.
pub struct SentTransactionHash(#[serde(with = "hex")] transaction::Hash);

#[derive(serde::Serialize)]
/// Response to a `getblock` RPC request.
pub struct GetBlock {
    #[serde(with = "hex")]
    data: SerializedBlock,
}
