//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `lightwalletd` client implementation.

use futures::{FutureExt, TryFutureExt};
use hex::{FromHex, ToHex};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    block::{self, SerializedBlock},
    chain_tip::ChainTip,
    parameters::Network,
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
    /// Returns software information from the RPC server, as a [`GetInfo`] JSON struct.
    ///
    /// zcashd reference: [`getinfo`](https://zcash.github.io/rpc/getinfo.html)
    ///
    /// # Notes
    ///
    /// [The zcashd reference](https://zcash.github.io/rpc/getinfo.html) might not show some fields
    /// in Zebra's [`GetInfo`]. Zebra uses the field names and formats from the
    /// [zcashd code](https://github.com/zcash/zcash/blob/v4.6.0-1/src/rpc/misc.cpp#L86-L87).
    ///
    /// Some fields from the zcashd reference are missing from Zebra's [`GetInfo`]. It only contains the fields
    /// [required for lightwalletd support.](https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L91-L95)
    #[rpc(name = "getinfo")]
    fn get_info(&self) -> Result<GetInfo>;

    /// Returns blockchain state information, as a [`GetBlockChainInfo`] JSON struct.
    ///
    /// zcashd reference: [`getblockchaininfo`](https://zcash.github.io/rpc/getblockchaininfo.html)
    ///
    /// TODO in the context of https://github.com/ZcashFoundation/zebra/issues/3143:
    /// - list the arguments and fields that lightwalletd uses
    /// - note any other lightwalletd changes
    #[rpc(name = "getblockchaininfo")]
    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo>;

    /// Sends the raw bytes of a signed transaction to the local node's mempool, if the transaction is valid.
    /// Returns the [`SentTransactionHash`] for the transaction, as a JSON string.
    ///
    /// zcashd reference: [`sendrawtransaction`](https://zcash.github.io/rpc/sendrawtransaction.html)
    ///
    /// # Parameters
    ///
    /// - `raw_transaction_hex`: (string, required) The hex-encoded raw transaction bytes.
    ///
    /// # Notes
    ///
    /// zcashd accepts an optional `allowhighfees` parameter. Zebra doesn't support this parameter,
    /// because lightwalletd doesn't use it.
    #[rpc(name = "sendrawtransaction")]
    fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
    ) -> BoxFuture<Result<SentTransactionHash>>;

    /// Returns the requested block by height, as a [`GetBlock`] JSON string.
    ///
    /// zcashd reference: [`getblock`](https://zcash.github.io/rpc/getblock.html)
    ///
    /// # Parameters
    ///
    /// - `height`: (string, required) The height number for the block to be returned.
    ///
    /// # Notes
    ///
    /// We only expose the `data` field as lightwalletd uses the non-verbose
    /// mode for all getblock calls: <https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L232>
    ///
    /// `lightwalletd` only requests blocks by height, so we don't support
    /// getting blocks by hash but we do need to send the height number as a string.
    ///
    /// The `verbosity` parameter is ignored but required in the call.
    #[rpc(name = "getblock")]
    fn get_block(&self, height: String, verbosity: u8) -> BoxFuture<Result<GetBlock>>;

    /// Returns the hash of the current best blockchain tip block, as a [`GetBestBlockHash`] JSON string.
    ///
    /// zcashd reference: [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)
    #[rpc(name = "getbestblockhash")]
    fn get_best_block_hash(&self) -> BoxFuture<Result<GetBestBlockHash>>;

    /// Returns all transaction ids in the memory pool, as a JSON array.
    ///
    /// zcashd reference: [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)
    #[rpc(name = "getrawmempool")]
    fn get_raw_mempool(&self) -> BoxFuture<Result<Vec<String>>>;
}

/// RPC method implementations.
pub struct RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>,
    State: Service<
        zebra_state::Request,
        Response = zebra_state::Response,
        Error = zebra_state::BoxError,
    >,
    Tip: ChainTip,
{
    /// Zebra's application version.
    app_version: String,

    /// A handle to the mempool service.
    mempool: Buffer<Mempool, mempool::Request>,

    /// A handle to the state service.
    state: State,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    /// The configured network for this RPC service.
    network: Network,
}

impl<Mempool, State, Tip> RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>,
    State: Service<
        zebra_state::Request,
        Response = zebra_state::Response,
        Error = zebra_state::BoxError,
    >,
    Tip: ChainTip + Send + Sync,
{
    /// Create a new instance of the RPC handler.
    pub fn new<Version>(
        app_version: Version,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        latest_chain_tip: Tip,
        network: Network,
    ) -> Self
    where
        Version: ToString,
    {
        RpcImpl {
            app_version: app_version.to_string(),
            mempool,
            state,
            latest_chain_tip,
            network,
        }
    }
}

impl<Mempool, State, Tip> Rpc for RpcImpl<Mempool, State, Tip>
where
    Mempool:
        tower::Service<mempool::Request, Response = mempool::Response, Error = BoxError> + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Send + Sync + 'static,
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
        //       use self.latest_chain_tip.estimate_network_chain_tip_height()
        //       to estimate the current block height on the network
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
                zebra_state::Response::Block(Some(block)) => Ok(GetBlock(block.into())),
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

    fn get_best_block_hash(&self) -> BoxFuture<Result<GetBestBlockHash>> {
        let mut state = self.state.clone();

        async move {
            let request = zebra_state::Request::Tip;
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
                zebra_state::Response::Tip(Some((_height, hash))) => Ok(GetBestBlockHash(hash)),
                zebra_state::Response::Tip(None) => Err(Error {
                    code: ErrorCode::ServerError(0),
                    message: "No blocks in state".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a tip request"),
            }
        }
        .boxed()
    }

    fn get_raw_mempool(&self) -> BoxFuture<Result<Vec<String>>> {
        let mut mempool = self.mempool.clone();

        async move {
            let request = mempool::Request::TransactionIds;

            let response = mempool
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            match response {
                mempool::Response::TransactionIds(unmined_transaction_ids) => {
                    Ok(unmined_transaction_ids
                        .iter()
                        .map(|id| id.mined_id().encode_hex())
                        .collect())
                }
                _ => unreachable!("unmatched response to a transactionids request"),
            }
        }
        .boxed()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getinfo` RPC request.
///
/// See the notes for the [`Rpc::get_info` method].
pub struct GetInfo {
    build: String,
    subversion: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
/// Response to a `getblockchaininfo` RPC request.
///
/// See the notes for the [`Rpc::get_blockchain_info` method].
pub struct GetBlockChainInfo {
    chain: String,
    // TODO: add other fields used by lightwalletd (#3143)
}

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
/// Response to a `sendrawtransaction` RPC request.
///
/// Contains the hex-encoded hash of the sent transaction.
///
/// See the notes for the [`Rpc::send_raw_transaction` method].
pub struct SentTransactionHash(#[serde(with = "hex")] transaction::Hash);

#[derive(serde::Serialize)]
/// Response to a `getblock` RPC request.
///
/// See the notes for the [`Rpc::get_block` method].
pub struct GetBlock(#[serde(with = "hex")] SerializedBlock);

#[derive(Debug, PartialEq, serde::Serialize)]
/// Response to a `getbestblockhash` RPC request.
///
/// Contains the hex-encoded hash of the tip block.
///
/// Also see the notes for the [`Rpc::get_best_block_hash` method].
pub struct GetBestBlockHash(#[serde(with = "hex")] block::Hash);
