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
    amount::{Amount, NonNegative},
    block::{self, Height, SerializedBlock},
    chain_tip::ChainTip,
    parameters::{ConsensusBranchId, Network, NetworkUpgrade},
    serialization::{SerializationError, ZcashDeserialize},
    transaction::{self, SerializedTransaction, Transaction, UnminedTx},
    transparent::Address,
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::{mempool, BoxError};

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
    ///
    /// # Notes
    ///
    /// We only expose the `data` field as lightwalletd uses the non-verbose
    /// mode for all getblock calls: <https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L232>
    ///
    /// `lightwalletd` only requests blocks by height, so we don't support
    /// getting blocks by hash. (But we parse the height as a JSON string, not an integer).
    ///
    /// The `verbosity` parameter is ignored but required in the call.
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
    /// - `addresses`: (json array of string, required) The addresses to get transactions from.
    /// - `start`: (numeric, required) The lower height to start looking for transactions (inclusive).
    /// - `end`: (numeric, required) The top height to stop looking for transactions (inclusive).
    ///
    /// # Notes
    ///
    /// Only the multi-argument format is used by lightwalletd and this is what we currently support:
    /// https://github.com/zcash/lightwalletd/blob/631bb16404e3d8b045e74a7c5489db626790b2f6/common/common.go#L97-L102
    #[rpc(name = "getaddresstxids")]
    fn get_address_tx_ids(
        &self,
        addresses: Vec<String>,
        start: u32,
        end: u32,
    ) -> BoxFuture<Result<Vec<String>>>;
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

        let rpc_impl = RpcImpl {
            app_version: app_version.to_string(),
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
            blocks: tip_height.0,
            best_block_hash: GetBestBlockHash(tip_hash),
            estimated_height: estimated_height.0,
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
            let addresses: HashSet<Address> = address_strings
                .addresses
                .into_iter()
                .map(|address| {
                    address.parse().map_err(|error| {
                        Error::invalid_params(&format!("invalid address {address:?}: {error}"))
                    })
                })
                .collect::<Result<_>>()?;

            let request = zebra_state::ReadRequest::AddressBalance(addresses);
            let response = state.oneshot(request).await.map_err(|error| Error {
                code: ErrorCode::ServerError(0),
                message: error.to_string(),
                data: None,
            })?;

            match response {
                zebra_state::ReadResponse::AddressBalance(balance) => {
                    Ok(AddressBalance { balance })
                }
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

    fn get_block(&self, height: String, _verbosity: u8) -> BoxFuture<Result<GetBlock>> {
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
                zebra_state::ReadResponse::Block(Some(block)) => Ok(GetBlock(block.into())),
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

    fn get_address_tx_ids(
        &self,
        addresses: Vec<String>,
        start: u32,
        end: u32,
    ) -> BoxFuture<Result<Vec<String>>> {
        let mut state = self.state.clone();
        let start = Height(start);
        let end = Height(end);

        let chain_height = self.latest_chain_tip.best_tip_height().ok_or(Error {
            code: ErrorCode::ServerError(0),
            message: "No blocks in state".to_string(),
            data: None,
        });

        async move {
            // height range checks
            check_height_range(start, end, chain_height?)?;

            let valid_addresses: Result<HashSet<Address>> = addresses
                .iter()
                .map(|address| {
                    address.parse().map_err(|_| {
                        Error::invalid_params(format!("Provided address is not valid: {}", address))
                    })
                })
                .collect();

            let request = zebra_state::ReadRequest::TransactionIdsByAddresses {
                addresses: valid_addresses?,
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
                    hashes.values().map(|tx_id| tx_id.to_string()).collect()
                }
                _ => unreachable!("unmatched response to a TransactionsByAddresses request"),
            };

            Ok(hashes)
        }
        .boxed()
    }
}

/// Response to a `getinfo` RPC request.
///
/// See the notes for the [`Rpc::get_info` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetInfo {
    build: String,
    subversion: String,
}

/// Response to a `getblockchaininfo` RPC request.
///
/// See the notes for the [`Rpc::get_blockchain_info` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockChainInfo {
    chain: String,
    blocks: u32,
    #[serde(rename = "bestblockhash")]
    best_block_hash: GetBestBlockHash,
    #[serde(rename = "estimatedheight")]
    estimated_height: u32,
    upgrades: IndexMap<ConsensusBranchIdHex, NetworkUpgradeInfo>,
    consensus: TipConsensusBranch,
}

/// A wrapper type with a list of strings of addresses.
///
/// This is used for the input parameter of [`Rpc::get_account_balance`].
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Deserialize)]
pub struct AddressStrings {
    addresses: Vec<String>,
}

/// The transparent balance of a set of addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct AddressBalance {
    balance: Amount<NonNegative>,
}

/// A hex-encoded [`ConsensusBranchId`] string.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
struct ConsensusBranchIdHex(#[serde(with = "hex")] ConsensusBranchId);

/// Information about [`NetworkUpgrade`] activation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct NetworkUpgradeInfo {
    name: NetworkUpgrade,
    #[serde(rename = "activationheight")]
    activation_height: Height,
    status: NetworkUpgradeStatus,
}

/// The activation status of a [`NetworkUpgrade`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum NetworkUpgradeStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "pending")]
    Pending,
}

/// The [`ConsensusBranchId`]s for the tip and the next block.
///
/// These branch IDs are different when the next block is a network upgrade activation block.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct TipConsensusBranch {
    #[serde(rename = "chaintip")]
    chain_tip: ConsensusBranchIdHex,
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
pub struct GetBlock(#[serde(with = "hex")] SerializedBlock);

/// Response to a `getbestblockhash` RPC request.
///
/// Contains the hex-encoded hash of the tip block.
///
/// Also see the notes for the [`Rpc::get_best_block_hash` method].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GetBestBlockHash(#[serde(with = "hex")] block::Hash);

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

impl GetRawTransaction {
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

/// Check if provided height range is valid
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
