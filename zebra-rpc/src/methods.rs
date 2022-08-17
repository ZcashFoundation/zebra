//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `zcashd` server and `lightwalletd` client implementations.

use std::{collections::HashSet, io, sync::Arc};

use chrono::Utc;
use futures::{FutureExt, TryFutureExt};
use hex::{FromHex, ToHex};
use indexmap::IndexMap;
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tokio::{sync::broadcast::Sender, task::JoinHandle};
use tower::{buffer::Buffer, Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    block::{self, Height, SerializedBlock},
    chain_tip::ChainTip,
    orchard,
    parameters::{ConsensusBranchId, Network, NetworkUpgrade},
    sapling,
    serialization::{SerializationError, ZcashDeserialize},
    transaction::{self, SerializedTransaction, Transaction, UnminedTx},
    transparent::{self, Address},
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::{mempool, BoxError};
use zebra_state::{OutputIndex, OutputLocation, TransactionLocation};

use crate::queue::Queue;

#[cfg(test)]
mod tests;

/// The RPC error code used by `zcashd` for missing blocks.
///
/// `lightwalletd` expects error code `-8` when a block is not found:
/// <https://github.com/adityapk00/lightwalletd/blob/c1bab818a683e4de69cd952317000f9bb2932274/common/common.go#L251-L254>
pub const MISSING_BLOCK_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-8);

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
    /// # Notes
    ///
    /// Some fields from the zcashd reference are missing from Zebra's [`GetBlockChainInfo`]. It only contains the fields
    /// [required for lightwalletd support.](https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L72-L89)
    #[rpc(name = "getblockchaininfo")]
    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo>;

    /// Returns the total balance of a provided `addresses` in an [`AddressBalance`] instance.
    ///
    /// zcashd reference: [`getaddressbalance`](https://zcash.github.io/rpc/getaddressbalance.html)
    ///
    /// # Parameters
    ///
    /// - `address_strings`: (map) A JSON map with a single entry
    ///   - `addresses`: (array of strings) A list of base-58 encoded addresses.
    ///
    /// # Notes
    ///
    /// zcashd also accepts a single string parameter instead of an array of strings, but Zebra
    /// doesn't because lightwalletd always calls this RPC with an array of addresses.
    ///
    /// zcashd also returns the total amount of Zatoshis received by the addresses, but Zebra
    /// doesn't because lightwalletd doesn't use that information.
    ///
    /// The RPC documentation says that the returned object has a string `balance` field, but
    /// zcashd actually [returns an
    /// integer](https://github.com/zcash/lightwalletd/blob/bdaac63f3ee0dbef62bde04f6817a9f90d483b00/common/common.go#L128-L130).
    #[rpc(name = "getaddressbalance")]
    fn get_address_balance(
        &self,
        address_strings: AddressStrings,
    ) -> BoxFuture<Result<AddressBalance>>;

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
    /// If the block is not in Zebra's state, returns
    /// [error code `-8`.](https://github.com/zcash/zcash/issues/5758)
    ///
    /// zcashd reference: [`getblock`](https://zcash.github.io/rpc/getblock.html)
    ///
    /// # Parameters
    ///
    /// - `height`: (string, required) The height number for the block to be returned.
    /// - `verbosity`: (numeric, optional, default=1) 0 for hex encoded data, 1 for a json object,
    ///     and 2 for json object with transaction data.
    ///
    /// # Notes
    ///
    /// With verbosity=1, [`lightwalletd` only reads the `tx` field of the
    /// result](https://github.com/zcash/lightwalletd/blob/dfac02093d85fb31fb9a8475b884dd6abca966c7/common/common.go#L152),
    /// so we only return that for now.
    ///
    /// `lightwalletd` only requests blocks by height, so we don't support
    /// getting blocks by hash. (But we parse the height as a JSON string, not an integer).
    /// `lightwalletd` also does not use verbosity=2, so we don't support it.
    #[rpc(name = "getblock")]
    fn get_block(&self, height: String, verbosity: u8) -> BoxFuture<Result<GetBlock>>;

    /// Returns the hash of the current best blockchain tip block, as a [`GetBestBlockHash`] JSON string.
    ///
    /// zcashd reference: [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)
    #[rpc(name = "getbestblockhash")]
    fn get_best_block_hash(&self) -> Result<GetBestBlockHash>;

    /// Returns all transaction ids in the memory pool, as a JSON array.
    ///
    /// zcashd reference: [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)
    #[rpc(name = "getrawmempool")]
    fn get_raw_mempool(&self) -> BoxFuture<Result<Vec<String>>>;

    /// Returns information about the given block's Sapling & Orchard tree state.
    ///
    /// zcashd reference: [`z_gettreestate`](https://zcash.github.io/rpc/z_gettreestate.html)
    ///
    /// # Parameters
    ///
    /// - `hash | height`: (string, required) The block hash or height.
    ///
    /// # Notes
    ///
    /// The zcashd doc reference above says that the parameter "`height` can be
    /// negative where -1 is the last known valid block". On the other hand,
    /// `lightwalletd` only uses positive heights, so Zebra does not support
    /// negative heights.
    #[rpc(name = "z_gettreestate")]
    fn z_get_treestate(&self, hash_or_height: String) -> BoxFuture<Result<GetTreestate>>;

    /// Returns the raw transaction data, as a [`GetRawTransaction`] JSON string or structure.
    ///
    /// zcashd reference: [`getrawtransaction`](https://zcash.github.io/rpc/getrawtransaction.html)
    ///
    /// # Parameters
    ///
    /// - `txid`: (string, required) The transaction ID of the transaction to be returned.
    /// - `verbose`: (numeric, optional, default=0) If 0, return a string of hex-encoded data, otherwise return a JSON object.
    ///
    /// # Notes
    ///
    /// We don't currently support the `blockhash` parameter since lightwalletd does not
    /// use it.
    ///
    /// In verbose mode, we only expose the `hex` and `height` fields since
    /// lightwalletd uses only those:
    /// <https://github.com/zcash/lightwalletd/blob/631bb16404e3d8b045e74a7c5489db626790b2f6/common/common.go#L119>
    #[rpc(name = "getrawtransaction")]
    fn get_raw_transaction(
        &self,
        txid_hex: String,
        verbose: u8,
    ) -> BoxFuture<Result<GetRawTransaction>>;

    /// Returns the transaction ids made by the provided transparent addresses.
    ///
    /// zcashd reference: [`getaddresstxids`](https://zcash.github.io/rpc/getaddresstxids.html)
    ///
    /// # Parameters
    ///
    /// A [`GetAddressTxIdsRequest`] struct with the following named fields:
    /// - `addresses`: (json array of string, required) The addresses to get transactions from.
    /// - `start`: (numeric, required) The lower height to start looking for transactions (inclusive).
    /// - `end`: (numeric, required) The top height to stop looking for transactions (inclusive).
    ///
    /// # Notes
    ///
    /// Only the multi-argument format is used by lightwalletd and this is what we currently support:
    /// <https://github.com/zcash/lightwalletd/blob/631bb16404e3d8b045e74a7c5489db626790b2f6/common/common.go#L97-L102>
    #[rpc(name = "getaddresstxids")]
    fn get_address_tx_ids(&self, request: GetAddressTxIdsRequest)
        -> BoxFuture<Result<Vec<String>>>;

    /// Returns all unspent outputs for a list of addresses.
    ///
    /// zcashd reference: [`getaddressutxos`](https://zcash.github.io/rpc/getaddressutxos.html)
    ///
    /// # Parameters
    ///
    /// - `addresses`: (json array of string, required) The addresses to get outputs from.
    ///
    /// # Notes
    ///
    /// lightwalletd always uses the multi-address request, without chaininfo:
    /// <https://github.com/zcash/lightwalletd/blob/master/frontend/service.go#L402>
    #[rpc(name = "getaddressutxos")]
    fn get_address_utxos(
        &self,
        address_strings: AddressStrings,
    ) -> BoxFuture<Result<Vec<GetAddressUtxos>>>;
}

/// RPC method implementations.
pub struct RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>,
    State: Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
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
    #[allow(dead_code)]
    network: Network,

    /// A sender component of a channel used to send transactions to the queue.
    queue_sender: Sender<Option<UnminedTx>>,
}

impl<Mempool, State, Tip> RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError> + 'static,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the RPC handler.
    pub fn new<Version>(
        app_version: Version,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        latest_chain_tip: Tip,
        network: Network,
    ) -> (Self, JoinHandle<()>)
    where
        Version: ToString,
        <Mempool as Service<mempool::Request>>::Future: Send,
        <State as Service<zebra_state::ReadRequest>>::Future: Send,
    {
        let runner = Queue::start();

        let mut app_version = app_version.to_string();

        // Match zcashd's version format, if the version string has anything in it
        if !app_version.is_empty() && !app_version.starts_with('v') {
            app_version.insert(0, 'v');
        }

        let rpc_impl = RpcImpl {
            app_version,
            mempool: mempool.clone(),
            state: state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            network,
            queue_sender: runner.sender(),
        };

        // run the process queue
        let rpc_tx_queue_task_handle = tokio::spawn(
            runner
                .run(mempool, state, latest_chain_tip, network)
                .in_current_span(),
        );

        (rpc_impl, rpc_tx_queue_task_handle)
    }
}

impl<Mempool, State, Tip> Rpc for RpcImpl<Mempool, State, Tip>
where
    Mempool:
        tower::Service<mempool::Request, Response = mempool::Response, Error = BoxError> + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
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

    #[allow(clippy::unwrap_in_result)]
    fn get_blockchain_info(&self) -> Result<GetBlockChainInfo> {
        let network = self.network;

        // `chain` field
        let chain = self.network.bip70_network_name();

        // `blocks` and `best_block_hash` fields
        let (tip_height, tip_hash) = self
            .latest_chain_tip
            .best_tip_height_and_hash()
            .ok_or_else(|| Error {
                code: ErrorCode::ServerError(0),
                message: "No Chain tip available yet".to_string(),
                data: None,
            })?;

        // `estimated_height` field
        let current_block_time =
            self.latest_chain_tip
                .best_tip_block_time()
                .ok_or_else(|| Error {
                    code: ErrorCode::ServerError(0),
                    message: "No Chain tip available yet".to_string(),
                    data: None,
                })?;

        let zebra_estimated_height = self
            .latest_chain_tip
            .estimate_network_chain_tip_height(network, Utc::now())
            .ok_or_else(|| Error {
                code: ErrorCode::ServerError(0),
                message: "No Chain tip available yet".to_string(),
                data: None,
            })?;

        let estimated_height =
            if current_block_time > Utc::now() || zebra_estimated_height < tip_height {
                tip_height
            } else {
                zebra_estimated_height
            };

        // `upgrades` object
        //
        // Get the network upgrades in height order, like `zcashd`.
        let mut upgrades = IndexMap::new();
        for (activation_height, network_upgrade) in NetworkUpgrade::activation_list(network) {
            // Zebra defines network upgrades based on incompatible consensus rule changes,
            // but zcashd defines them based on ZIPs.
            //
            // All the network upgrades with a consensus branch ID are the same in Zebra and zcashd.
            if let Some(branch_id) = network_upgrade.branch_id() {
                // zcashd's RPC seems to ignore Disabled network upgrades, so Zebra does too.
                let status = if tip_height >= activation_height {
                    NetworkUpgradeStatus::Active
                } else {
                    NetworkUpgradeStatus::Pending
                };

                let upgrade = NetworkUpgradeInfo {
                    name: network_upgrade,
                    activation_height,
                    status,
                };
                upgrades.insert(ConsensusBranchIdHex(branch_id), upgrade);
            }
        }

        // `consensus` object
        let next_block_height =
            (tip_height + 1).expect("valid chain tips are a lot less than Height::MAX");
        let consensus = TipConsensusBranch {
            chain_tip: ConsensusBranchIdHex(
                NetworkUpgrade::current(network, tip_height)
                    .branch_id()
                    .unwrap_or(ConsensusBranchId::RPC_MISSING_ID),
            ),
            next_block: ConsensusBranchIdHex(
                NetworkUpgrade::current(network, next_block_height)
                    .branch_id()
                    .unwrap_or(ConsensusBranchId::RPC_MISSING_ID),
            ),
        };

        let response = GetBlockChainInfo {
            chain,
            blocks: tip_height,
            best_block_hash: tip_hash,
            estimated_height,
            upgrades,
            consensus,
        };

        Ok(response)
    }

    fn get_address_balance(
        &self,
        address_strings: AddressStrings,
    ) -> BoxFuture<Result<AddressBalance>> {
        let state = self.state.clone();

        async move {
            let valid_addresses = address_strings.valid_addresses()?;

            let request = zebra_state::ReadRequest::AddressBalance(valid_addresses);
            let response = state.oneshot(request).await.map_err(|error| Error {
                code: ErrorCode::ServerError(0),
                message: error.to_string(),
                data: None,
            })?;

            match response {
                zebra_state::ReadResponse::AddressBalance(balance) => Ok(AddressBalance {
                    balance: u64::from(balance),
                }),
                _ => unreachable!("Unexpected response from state service: {response:?}"),
            }
        }
        .boxed()
    }

    fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
    ) -> BoxFuture<Result<SentTransactionHash>> {
        let mempool = self.mempool.clone();
        let queue_sender = self.queue_sender.clone();

        async move {
            let raw_transaction_bytes = Vec::from_hex(raw_transaction_hex).map_err(|_| {
                Error::invalid_params("raw transaction is not specified as a hex string")
            })?;
            let raw_transaction = Transaction::zcash_deserialize(&*raw_transaction_bytes)
                .map_err(|_| Error::invalid_params("raw transaction is structurally invalid"))?;

            let transaction_hash = raw_transaction.hash();

            // send transaction to the rpc queue, ignore any error.
            let unmined_transaction = UnminedTx::from(raw_transaction.clone());
            let _ = queue_sender.send(Some(unmined_transaction));

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

    fn get_block(&self, height: String, verbosity: u8) -> BoxFuture<Result<GetBlock>> {
        let mut state = self.state.clone();

        async move {
            let height = height.parse().map_err(|error: SerializationError| Error {
                code: ErrorCode::ServerError(0),
                message: error.to_string(),
                data: None,
            })?;

            let request =
                zebra_state::ReadRequest::Block(zebra_state::HashOrHeight::Height(height));
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
                zebra_state::ReadResponse::Block(Some(block)) => match verbosity {
                    0 => Ok(GetBlock::Raw(block.into())),
                    1 => Ok(GetBlock::Object {
                        tx: block
                            .transactions
                            .iter()
                            .map(|tx| tx.hash().encode_hex())
                            .collect(),
                    }),
                    _ => Err(Error {
                        code: ErrorCode::InvalidParams,
                        message: "Invalid verbosity value".to_string(),
                        data: None,
                    }),
                },
                zebra_state::ReadResponse::Block(None) => Err(Error {
                    code: MISSING_BLOCK_ERROR_CODE,
                    message: "Block not found".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a block request"),
            }
        }
        .boxed()
    }

    fn get_best_block_hash(&self) -> Result<GetBestBlockHash> {
        self.latest_chain_tip
            .best_tip_hash()
            .map(GetBestBlockHash)
            .ok_or(Error {
                code: ErrorCode::ServerError(0),
                message: "No blocks in state".to_string(),
                data: None,
            })
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
                    let mut tx_ids: Vec<String> = unmined_transaction_ids
                        .iter()
                        .map(|id| id.mined_id().encode_hex())
                        .collect();

                    // Sort returned transaction IDs in numeric/string order.
                    // (zcashd's sort order appears arbitrary.)
                    tx_ids.sort();

                    Ok(tx_ids)
                }
                _ => unreachable!("unmatched response to a transactionids request"),
            }
        }
        .boxed()
    }

    fn get_raw_transaction(
        &self,
        txid_hex: String,
        verbose: u8,
    ) -> BoxFuture<Result<GetRawTransaction>> {
        let mut state = self.state.clone();
        let mut mempool = self.mempool.clone();

        async move {
            let txid = transaction::Hash::from_hex(txid_hex).map_err(|_| {
                Error::invalid_params("transaction ID is not specified as a hex string")
            })?;

            // Check the mempool first.
            //
            // # Correctness
            //
            // Transactions are removed from the mempool after they are mined into blocks,
            // so the transaction could be just in the mempool, just in the state, or in both.
            // (And the mempool and state transactions could have different authorising data.)
            // But it doesn't matter which transaction we choose, because the effects are the same.
            let mut txid_set = HashSet::new();
            txid_set.insert(txid);
            let request = mempool::Request::TransactionsByMinedId(txid_set);

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
                mempool::Response::Transactions(unmined_transactions) => {
                    if !unmined_transactions.is_empty() {
                        let tx = unmined_transactions[0].transaction.clone();
                        return GetRawTransaction::from_transaction(tx, None, verbose != 0)
                            .map_err(|error| Error {
                                code: ErrorCode::ServerError(0),
                                message: error.to_string(),
                                data: None,
                            });
                    }
                }
                _ => unreachable!("unmatched response to a transactionids request"),
            };

            // Now check the state
            let request = zebra_state::ReadRequest::Transaction(txid);
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
                zebra_state::ReadResponse::Transaction(Some((tx, height))) => Ok(
                    GetRawTransaction::from_transaction(tx, Some(height), verbose != 0).map_err(
                        |error| Error {
                            code: ErrorCode::ServerError(0),
                            message: error.to_string(),
                            data: None,
                        },
                    )?,
                ),
                zebra_state::ReadResponse::Transaction(None) => Err(Error {
                    code: ErrorCode::ServerError(0),
                    message: "Transaction not found".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a transaction request"),
            }
        }
        .boxed()
    }

    fn z_get_treestate(&self, hash_or_height: String) -> BoxFuture<Result<GetTreestate>> {
        let mut state = self.state.clone();

        async move {
            // Convert the [`hash_or_height`] string into an actual hash or height.
            let hash_or_height = hash_or_height
                .parse()
                .map_err(|error: SerializationError| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            // Fetch the block referenced by [`hash_or_height`] from the state.

            // TODO: If this RPC is called a lot, just get the block header,
            // rather than the whole block.
            let block_request = zebra_state::ReadRequest::Block(hash_or_height);
            let block_response = state
                .ready()
                .and_then(|service| service.call(block_request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            // The block hash, height, and time are all required fields in the
            // RPC response. For this reason, we throw an error early if the
            // state didn't return the requested block so that we prevent
            // further state queries.
            let block = match block_response {
                zebra_state::ReadResponse::Block(Some(block)) => block,
                zebra_state::ReadResponse::Block(None) => {
                    return Err(Error {
                        code: ErrorCode::ServerError(0),
                        message: "the requested block was not found".to_string(),
                        data: None,
                    })
                }
                _ => unreachable!("unmatched response to a block request"),
            };

            // Fetch the Sapling & Orchard treestates referenced by
            // [`hash_or_height`] from the state.

            let sapling_request = zebra_state::ReadRequest::SaplingTree(hash_or_height);
            let sapling_response = state
                .ready()
                .and_then(|service| service.call(sapling_request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            let orchard_request = zebra_state::ReadRequest::OrchardTree(hash_or_height);
            let orchard_response = state
                .ready()
                .and_then(|service| service.call(orchard_request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            // We've got all the data we need for the RPC response, so we
            // assemble the response.

            let hash = block.hash();

            let height = block
                .coinbase_height()
                .expect("verified blocks have a valid height");

            let time = u32::try_from(block.header.time.timestamp())
                .expect("Timestamps of valid blocks always fit into u32.");

            let sapling_tree = match sapling_response {
                zebra_state::ReadResponse::SaplingTree(maybe_tree) => {
                    sapling::tree::SerializedTree::from(maybe_tree)
                }
                _ => unreachable!("unmatched response to a sapling tree request"),
            };

            let orchard_tree = match orchard_response {
                zebra_state::ReadResponse::OrchardTree(maybe_tree) => {
                    orchard::tree::SerializedTree::from(maybe_tree)
                }
                _ => unreachable!("unmatched response to an orchard tree request"),
            };

            Ok(GetTreestate {
                hash,
                height,
                time,
                sapling: Treestate {
                    commitments: Commitments {
                        final_state: sapling_tree,
                    },
                },
                orchard: Treestate {
                    commitments: Commitments {
                        final_state: orchard_tree,
                    },
                },
            })
        }
        .boxed()
    }

    fn get_address_tx_ids(
        &self,
        request: GetAddressTxIdsRequest,
    ) -> BoxFuture<Result<Vec<String>>> {
        let mut state = self.state.clone();
        let start = Height(request.start);
        let end = Height(request.end);

        let chain_height = self.latest_chain_tip.best_tip_height().ok_or(Error {
            code: ErrorCode::ServerError(0),
            message: "No blocks in state".to_string(),
            data: None,
        });

        async move {
            // height range checks
            check_height_range(start, end, chain_height?)?;

            let valid_addresses = AddressStrings {
                addresses: request.addresses,
            }
            .valid_addresses()?;

            let request = zebra_state::ReadRequest::TransactionIdsByAddresses {
                addresses: valid_addresses,
                height_range: start..=end,
            };
            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            let hashes = match response {
                zebra_state::ReadResponse::AddressesTransactionIds(hashes) => {
                    let mut last_tx_location = TransactionLocation::from_usize(Height(0), 0);

                    hashes
                        .iter()
                        .map(|(tx_loc, tx_id)| {
                            // TODO: downgrade to debug, because there's nothing the user can do
                            assert!(
                                *tx_loc > last_tx_location,
                                "Transactions were not in chain order:\n\
                                 {tx_loc:?} {tx_id:?} was after:\n\
                                 {last_tx_location:?}",
                            );

                            last_tx_location = *tx_loc;

                            tx_id.to_string()
                        })
                        .collect()
                }
                _ => unreachable!("unmatched response to a TransactionsByAddresses request"),
            };

            Ok(hashes)
        }
        .boxed()
    }

    fn get_address_utxos(
        &self,
        address_strings: AddressStrings,
    ) -> BoxFuture<Result<Vec<GetAddressUtxos>>> {
        let mut state = self.state.clone();
        let mut response_utxos = vec![];

        async move {
            let valid_addresses = address_strings.valid_addresses()?;

            // get utxos data for addresses
            let request = zebra_state::ReadRequest::UtxosByAddresses(valid_addresses);
            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;
            let utxos = match response {
                zebra_state::ReadResponse::Utxos(utxos) => utxos,
                _ => unreachable!("unmatched response to a UtxosByAddresses request"),
            };

            let mut last_output_location = OutputLocation::from_usize(Height(0), 0, 0);

            for utxo_data in utxos.utxos() {
                let address = utxo_data.0;
                let txid = *utxo_data.1;
                let height = utxo_data.2.height();
                let output_index = utxo_data.2.output_index();
                let script = utxo_data.3.lock_script.clone();
                let satoshis = u64::from(utxo_data.3.value);

                let output_location = *utxo_data.2;
                // TODO: downgrade to debug, because there's nothing the user can do
                assert!(
                    output_location > last_output_location,
                    "UTXOs were not in chain order:\n\
                     {output_location:?} {address:?} {txid:?} was after:\n\
                     {last_output_location:?}",
                );

                let entry = GetAddressUtxos {
                    address,
                    txid,
                    output_index,
                    script,
                    satoshis,
                    height,
                };
                response_utxos.push(entry);

                last_output_location = output_location;
            }

            Ok(response_utxos)
        }
        .boxed()
    }
}

/// Response to a `getinfo` RPC request.
///
/// See the notes for the [`Rpc::get_info` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetInfo {
    /// The node version build number
    build: String,

    /// The server sub-version identifier, used as the network protocol user-agent
    subversion: String,
}

/// Response to a `getblockchaininfo` RPC request.
///
/// See the notes for the [`Rpc::get_blockchain_info` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockChainInfo {
    /// Current network name as defined in BIP70 (main, test, regtest)
    chain: String,

    /// The current number of blocks processed in the server, numeric
    blocks: Height,

    /// The hash of the currently best block, in big-endian order, hex-encoded
    #[serde(rename = "bestblockhash", with = "hex")]
    best_block_hash: block::Hash,

    /// If syncing, the estimated height of the chain, else the current best height, numeric.
    ///
    /// In Zebra, this is always the height estimate, so it might be a little inaccurate.
    #[serde(rename = "estimatedheight")]
    estimated_height: Height,

    /// Status of network upgrades
    upgrades: IndexMap<ConsensusBranchIdHex, NetworkUpgradeInfo>,

    /// Branch IDs of the current and upcoming consensus rules
    consensus: TipConsensusBranch,
}

/// A wrapper type with a list of transparent address strings.
///
/// This is used for the input parameter of [`Rpc::get_address_balance`],
/// [`Rpc::get_address_tx_ids`] and [`Rpc::get_address_utxos`].
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Deserialize)]
pub struct AddressStrings {
    /// A list of transparent address strings.
    addresses: Vec<String>,
}

impl AddressStrings {
    /// Creates a new `AddressStrings` given a vector.
    #[cfg(test)]
    pub fn new(addresses: Vec<String>) -> AddressStrings {
        AddressStrings { addresses }
    }

    /// Given a list of addresses as strings:
    /// - check if provided list have all valid transparent addresses.
    /// - return valid addresses as a set of `Address`.
    pub fn valid_addresses(self) -> Result<HashSet<Address>> {
        let valid_addresses: HashSet<Address> = self
            .addresses
            .into_iter()
            .map(|address| {
                address.parse().map_err(|error| {
                    Error::invalid_params(&format!("invalid address {address:?}: {error}"))
                })
            })
            .collect::<Result<_>>()?;

        Ok(valid_addresses)
    }
}

/// The transparent balance of a set of addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct AddressBalance {
    /// The total transparent balance.
    balance: u64,
}

/// A hex-encoded [`ConsensusBranchId`] string.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
struct ConsensusBranchIdHex(#[serde(with = "hex")] ConsensusBranchId);

/// Information about [`NetworkUpgrade`] activation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct NetworkUpgradeInfo {
    /// Name of upgrade, string.
    ///
    /// Ignored by lightwalletd, but useful for debugging.
    name: NetworkUpgrade,

    /// Block height of activation, numeric.
    #[serde(rename = "activationheight")]
    activation_height: Height,

    /// Status of upgrade, string.
    status: NetworkUpgradeStatus,
}

/// The activation status of a [`NetworkUpgrade`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum NetworkUpgradeStatus {
    /// The network upgrade is currently active.
    ///
    /// Includes all network upgrades that have previously activated,
    /// even if they are not the most recent network upgrade.
    #[serde(rename = "active")]
    Active,

    /// The network upgrade does not have an activation height.
    #[serde(rename = "disabled")]
    Disabled,

    /// The network upgrade has an activation height, but we haven't reached it yet.
    #[serde(rename = "pending")]
    Pending,
}

/// The [`ConsensusBranchId`]s for the tip and the next block.
///
/// These branch IDs are different when the next block is a network upgrade activation block.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct TipConsensusBranch {
    /// Branch ID used to validate the current chain tip, big-endian, hex-encoded.
    #[serde(rename = "chaintip")]
    chain_tip: ConsensusBranchIdHex,

    /// Branch ID used to validate the next block, big-endian, hex-encoded.
    #[serde(rename = "nextblock")]
    next_block: ConsensusBranchIdHex,
}

/// Response to a `sendrawtransaction` RPC request.
///
/// Contains the hex-encoded hash of the sent transaction.
///
/// See the notes for the [`Rpc::send_raw_transaction` method].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SentTransactionHash(#[serde(with = "hex")] transaction::Hash);

/// Response to a `getblock` RPC request.
///
/// See the notes for the [`Rpc::get_block` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum GetBlock {
    /// The request block, hex-encoded.
    Raw(#[serde(with = "hex")] SerializedBlock),
    /// The block object.
    Object {
        /// Vector of hex-encoded TXIDs of the transactions of the block
        tx: Vec<String>,
    },
}

/// Response to a `getbestblockhash` RPC request.
///
/// Contains the hex-encoded hash of the tip block.
///
/// Also see the notes for the [`Rpc::get_best_block_hash` method].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GetBestBlockHash(#[serde(with = "hex")] block::Hash);

/// Response to a `z_gettreestate` RPC request.
///
/// Contains the hex-encoded Sapling & Orchard note commitment trees, and their
/// corresponding [`block::Hash`], [`Height`], and block time.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetTreestate {
    /// The block hash corresponding to the treestate, hex-encoded.
    #[serde(with = "hex")]
    hash: block::Hash,

    /// The block height corresponding to the treestate, numeric.
    height: Height,

    /// Unix time when the block corresponding to the treestate was mined,
    /// numeric.
    ///
    /// UTC seconds since the Unix 1970-01-01 epoch.
    time: u32,

    /// A treestate containing a Sapling note commitment tree, hex-encoded.
    #[serde(skip_serializing_if = "Treestate::is_empty")]
    sapling: Treestate<sapling::tree::SerializedTree>,

    /// A treestate containing an Orchard note commitment tree, hex-encoded.
    #[serde(skip_serializing_if = "Treestate::is_empty")]
    orchard: Treestate<orchard::tree::SerializedTree>,
}

/// A treestate that is included in the [`z_gettreestate`][1] RPC response.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
struct Treestate<Tree: AsRef<[u8]>> {
    /// Contains an Orchard or Sapling serialized note commitment tree,
    /// hex-encoded.
    commitments: Commitments<Tree>,
}

/// A wrapper that contains either an Orchard or Sapling note commitment tree.
///
/// Note that in the original [`z_gettreestate`][1] RPC, [`Commitments`] also
/// contains the field `finalRoot`. Zebra does *not* use this field.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
struct Commitments<Tree: AsRef<[u8]>> {
    /// Orchard or Sapling serialized note commitment tree, hex-encoded.
    #[serde(with = "hex")]
    #[serde(rename = "finalState")]
    final_state: Tree,
}

impl<Tree: AsRef<[u8]>> Treestate<Tree> {
    /// Returns `true` if there's no serialized commitment tree.
    fn is_empty(&self) -> bool {
        self.commitments.final_state.as_ref().is_empty()
    }
}

/// Response to a `getrawtransaction` RPC request.
///
/// See the notes for the [`Rpc::get_raw_transaction` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum GetRawTransaction {
    /// The raw transaction, encoded as hex bytes.
    Raw(#[serde(with = "hex")] SerializedTransaction),
    /// The transaction object.
    Object {
        /// The raw transaction, encoded as hex bytes.
        #[serde(with = "hex")]
        hex: SerializedTransaction,
        /// The height of the block that contains the transaction, or -1 if
        /// not applicable.
        height: i32,
    },
}

/// Response to a `getaddressutxos` RPC request.
///
/// See the notes for the [`Rpc::get_address_utxos` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetAddressUtxos {
    /// The transparent address, base58check encoded
    address: transparent::Address,

    /// The output txid, in big-endian order, hex-encoded
    #[serde(with = "hex")]
    txid: transaction::Hash,

    /// The transparent output index, numeric
    #[serde(rename = "outputIndex")]
    output_index: OutputIndex,

    /// The transparent output script, hex encoded
    #[serde(with = "hex")]
    script: transparent::Script,

    /// The amount of zatoshis in the transparent output
    satoshis: u64,

    /// The block height, numeric.
    ///
    /// We put this field last, to match the zcashd order.
    height: Height,
}

/// A struct to use as parameter of the `getaddresstxids`.
///
/// See the notes for the [`Rpc::get_address_tx_ids` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub struct GetAddressTxIdsRequest {
    // A list of addresses to get transactions from.
    addresses: Vec<String>,
    // The height to start looking for transactions.
    start: u32,
    // The height to end looking for transactions.
    end: u32,
}

impl GetRawTransaction {
    /// Converts `tx` and `height` into a new `GetRawTransaction` in the `verbose` format.
    #[allow(clippy::unwrap_in_result)]
    fn from_transaction(
        tx: Arc<Transaction>,
        height: Option<block::Height>,
        verbose: bool,
    ) -> std::result::Result<Self, io::Error> {
        if verbose {
            Ok(GetRawTransaction::Object {
                hex: tx.into(),
                height: match height {
                    Some(height) => height
                        .0
                        .try_into()
                        .expect("valid block heights are limited to i32::MAX"),
                    None => -1,
                },
            })
        } else {
            Ok(GetRawTransaction::Raw(tx.into()))
        }
    }
}

/// Check if provided height range is valid for address indexes.
fn check_height_range(start: Height, end: Height, chain_height: Height) -> Result<()> {
    if start == Height(0) || end == Height(0) {
        return Err(Error::invalid_params(
            "Start and end are expected to be greater than zero",
        ));
    }
    if end < start {
        return Err(Error::invalid_params(
            "End value is expected to be greater than or equal to start",
        ));
    }
    if start > chain_height || end > chain_height {
        return Err(Error::invalid_params("Start or end is outside chain range"));
    }

    Ok(())
}
