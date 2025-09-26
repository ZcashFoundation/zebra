//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `zcashd` server and `lightwalletd` client implementations.

use std::{collections::HashSet, fmt::Debug, sync::Arc};

use chrono::Utc;
use futures::{stream::FuturesOrdered, FutureExt, StreamExt, TryFutureExt};
use hex::{FromHex, ToHex};
use indexmap::IndexMap;
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zcash_primitives::consensus::Parameters;
use zebra_chain::{
    block::{self, Height, SerializedBlock},
    chain_tip::{ChainTip, NetworkChainTipHeightEstimator},
    parameters::{ConsensusBranchId, Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    subtree::NoteCommitmentSubtreeIndex,
    transaction::{self, SerializedTransaction, Transaction, UnminedTx},
    transparent::{self, Address},
};
use zebra_node_services::mempool;
use zebra_state::{HashOrHeight, MinedTx, OutputIndex, OutputLocation, TransactionLocation};

use crate::{
    constants::{INVALID_PARAMETERS_ERROR_CODE, MISSING_BLOCK_ERROR_CODE},
    methods::trees::{GetSubtrees, GetTreestate, SubtreeRpcData},
    queue::Queue,
};

mod errors;
pub mod hex_data;

use errors::{MapServerError, OkOrServerError};

// We don't use a types/ module here, because it is redundant.
pub mod trees;

pub mod types;

#[cfg(feature = "getblocktemplate-rpcs")]
pub mod get_block_template_rpcs;

#[cfg(feature = "getblocktemplate-rpcs")]
pub use get_block_template_rpcs::{GetBlockTemplateRpc, GetBlockTemplateRpcImpl};

#[cfg(test)]
mod tests;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
    #[rpc(name = "getinfo")]
    /// Returns software information from the RPC server, as a [`GetInfo`] JSON struct.
    ///
    /// zcashd reference: [`getinfo`](https://zcash.github.io/rpc/getinfo.html)
    /// method: post
    /// tags: control
    ///
    /// # Notes
    ///
    /// [The zcashd reference](https://zcash.github.io/rpc/getinfo.html) might not show some fields
    /// in Zebra's [`GetInfo`]. Zebra uses the field names and formats from the
    /// [zcashd code](https://github.com/zcash/zcash/blob/v4.6.0-1/src/rpc/misc.cpp#L86-L87).
    ///
    /// Some fields from the zcashd reference are missing from Zebra's [`GetInfo`]. It only contains the fields
    /// [required for lightwalletd support.](https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L91-L95)
    fn get_info(&self) -> Result<GetInfo>;

    /// Returns blockchain state information, as a [`GetBlockChainInfo`] JSON struct.
    ///
    /// zcashd reference: [`getblockchaininfo`](https://zcash.github.io/rpc/getblockchaininfo.html)
    /// method: post
    /// tags: blockchain
    ///
    /// # Notes
    ///
    /// Some fields from the zcashd reference are missing from Zebra's [`GetBlockChainInfo`]. It only contains the fields
    /// [required for lightwalletd support.](https://github.com/zcash/lightwalletd/blob/v0.4.9/common/common.go#L72-L89)
    #[rpc(name = "getblockchaininfo")]
    fn get_blockchain_info(&self) -> BoxFuture<Result<GetBlockChainInfo>>;

    /// Returns the total balance of a provided `addresses` in an [`AddressBalance`] instance.
    ///
    /// zcashd reference: [`getaddressbalance`](https://zcash.github.io/rpc/getaddressbalance.html)
    /// method: post
    /// tags: address
    ///
    /// # Parameters
    ///
    /// - `address_strings`: (object, example={"addresses": ["tmYXBYJj1K7vhejSec5osXK2QsGa5MTisUQ"]}) A JSON map with a single entry
    ///     - `addresses`: (array of strings) A list of base-58 encoded addresses.
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
    /// method: post
    /// tags: transaction
    ///
    /// # Parameters
    ///
    /// - `raw_transaction_hex`: (string, required, example="signedhex") The hex-encoded raw transaction bytes.
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

    /// Returns the requested block by hash or height, as a [`GetBlock`] JSON string.
    /// If the block is not in Zebra's state, returns
    /// [error code `-8`.](https://github.com/zcash/zcash/issues/5758)
    ///
    /// zcashd reference: [`getblock`](https://zcash.github.io/rpc/getblock.html)
    /// method: post
    /// tags: blockchain
    ///
    /// # Parameters
    ///
    /// - `hash_or_height`: (string, required, example="1") The hash or height for the block to be returned.
    /// - `verbosity`: (number, optional, default=1, example=1) 0 for hex encoded data, 1 for a json object, and 2 for json object with transaction data.
    ///
    /// # Notes
    ///
    /// With verbosity=1, [`lightwalletd` only reads the `tx` field of the
    /// result](https://github.com/zcash/lightwalletd/blob/dfac02093d85fb31fb9a8475b884dd6abca966c7/common/common.go#L152),
    /// and other clients only read the `hash` and `confirmations` fields,
    /// so we only return a few fields for now.
    ///
    /// `lightwalletd` and mining clients also do not use verbosity=2, so we don't support it.
    #[rpc(name = "getblock")]
    fn get_block(
        &self,
        hash_or_height: String,
        verbosity: Option<u8>,
    ) -> BoxFuture<Result<GetBlock>>;

    /// Returns the hash of the current best blockchain tip block, as a [`GetBlockHash`] JSON string.
    ///
    /// zcashd reference: [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)
    /// method: post
    /// tags: blockchain
    #[rpc(name = "getbestblockhash")]
    fn get_best_block_hash(&self) -> Result<GetBlockHash>;

    /// Returns the height and hash of the current best blockchain tip block, as a [`GetBlockHeightAndHash`] JSON struct.
    ///
    /// zcashd reference: none
    /// method: post
    /// tags: blockchain
    #[rpc(name = "getbestblockheightandhash")]
    fn get_best_block_height_and_hash(&self) -> Result<GetBlockHeightAndHash>;

    /// Returns all transaction ids in the memory pool, as a JSON array.
    ///
    /// zcashd reference: [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)
    /// method: post
    /// tags: blockchain
    #[rpc(name = "getrawmempool")]
    fn get_raw_mempool(&self) -> BoxFuture<Result<Vec<String>>>;

    /// Returns information about the given block's Sapling & Orchard tree state.
    ///
    /// zcashd reference: [`z_gettreestate`](https://zcash.github.io/rpc/z_gettreestate.html)
    /// method: post
    /// tags: blockchain
    ///
    /// # Parameters
    ///
    /// - `hash | height`: (string, required, example="00000000febc373a1da2bd9f887b105ad79ddc26ac26c2b28652d64e5207c5b5") The block hash or height.
    ///
    /// # Notes
    ///
    /// The zcashd doc reference above says that the parameter "`height` can be
    /// negative where -1 is the last known valid block". On the other hand,
    /// `lightwalletd` only uses positive heights, so Zebra does not support
    /// negative heights.
    #[rpc(name = "z_gettreestate")]
    fn z_get_treestate(&self, hash_or_height: String) -> BoxFuture<Result<GetTreestate>>;

    /// Returns information about a range of Sapling or Orchard subtrees.
    ///
    /// zcashd reference: [`z_getsubtreesbyindex`](https://zcash.github.io/rpc/z_getsubtreesbyindex.html) - TODO: fix link
    /// method: post
    /// tags: blockchain
    ///
    /// # Parameters
    ///
    /// - `pool`: (string, required) The pool from which subtrees should be returned. Either "sapling" or "orchard".
    /// - `start_index`: (number, required) The index of the first 2^16-leaf subtree to return.
    /// - `limit`: (number, optional) The maximum number of subtree values to return.
    ///
    /// # Notes
    ///
    /// While Zebra is doing its initial subtree index rebuild, subtrees will become available
    /// starting at the chain tip. This RPC will return an empty list if the `start_index` subtree
    /// exists, but has not been rebuilt yet. This matches `zcashd`'s behaviour when subtrees aren't
    /// available yet. (But `zcashd` does its rebuild before syncing any blocks.)
    #[rpc(name = "z_getsubtreesbyindex")]
    fn z_get_subtrees_by_index(
        &self,
        pool: String,
        start_index: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> BoxFuture<Result<GetSubtrees>>;

    /// Returns the raw transaction data, as a [`GetRawTransaction`] JSON string or structure.
    ///
    /// zcashd reference: [`getrawtransaction`](https://zcash.github.io/rpc/getrawtransaction.html)
    /// method: post
    /// tags: transaction
    ///
    /// # Parameters
    ///
    /// - `txid`: (string, required, example="mytxid") The transaction ID of the transaction to be returned.
    /// - `verbose`: (number, optional, default=0, example=1) If 0, return a string of hex-encoded data, otherwise return a JSON object.
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
        verbose: Option<u8>,
    ) -> BoxFuture<Result<GetRawTransaction>>;

    /// Returns the transaction ids made by the provided transparent addresses.
    ///
    /// zcashd reference: [`getaddresstxids`](https://zcash.github.io/rpc/getaddresstxids.html)
    /// method: post
    /// tags: address
    ///
    /// # Parameters
    ///
    /// - `request`: (object, required, example={\"addresses\": [\"tmYXBYJj1K7vhejSec5osXK2QsGa5MTisUQ\"], \"start\": 1000, \"end\": 2000}) A struct with the following named fields:
    ///     - `addresses`: (json array of string, required) The addresses to get transactions from.
    ///     - `start`: (numeric, required) The lower height to start looking for transactions (inclusive).
    ///     - `end`: (numeric, required) The top height to stop looking for transactions (inclusive).
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
    /// method: post
    /// tags: address
    ///
    /// # Parameters
    ///
    /// - `addresses`: (array, required, example={\"addresses\": [\"tmYXBYJj1K7vhejSec5osXK2QsGa5MTisUQ\"]}) The addresses to get outputs from.
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

    /// Returns the asset state of the provided asset base at the best chain tip or finalized chain tip.
    ///
    /// method: post
    /// tags: blockchain
    // #[rpc(name = "getassetstate")]
    // fn get_asset_state(
    //     &self,
    //     asset_base: String,
    //     include_non_finalized: Option<bool>,
    // ) -> BoxFuture<Result<zebra_chain::orchard_zsa::AssetState>>;

    /// Stop the running zebrad process.
    ///
    /// # Notes
    ///
    /// - Works for non windows targets only.
    /// - Works only if the network of the running zebrad process is `Regtest`.
    ///
    /// zcashd reference: [`stop`](https://zcash.github.io/rpc/stop.html)
    /// method: post
    /// tags: control
    #[rpc(name = "stop")]
    fn stop(&self) -> Result<String>;
}

/// RPC method implementations.
#[derive(Clone)]
pub struct RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    // Configuration
    //
    /// Zebra's application version, with build metadata.
    build_version: String,

    /// Zebra's RPC user agent.
    user_agent: String,

    /// The configured network for this RPC service.
    network: Network,

    /// Test-only option that makes Zebra say it is at the chain tip,
    /// no matter what the estimated height or local clock is.
    debug_force_finished_sync: bool,

    /// Test-only option that makes RPC responses more like `zcashd`.
    #[allow(dead_code)]
    debug_like_zcashd: bool,

    // Services
    //
    /// A handle to the mempool service.
    mempool: Mempool,

    /// A handle to the state service.
    state: State,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    // Tasks
    //
    /// A sender component of a channel used to send transactions to the mempool queue.
    queue_sender: broadcast::Sender<UnminedTx>,
}

impl<Mempool, State, Tip> Debug for RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Skip fields without Debug impls, and skip channels
        f.debug_struct("RpcImpl")
            .field("build_version", &self.build_version)
            .field("user_agent", &self.user_agent)
            .field("network", &self.network)
            .field("debug_force_finished_sync", &self.debug_force_finished_sync)
            .field("debug_like_zcashd", &self.debug_like_zcashd)
            .finish()
    }
}

impl<Mempool, State, Tip> RpcImpl<Mempool, State, Tip>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the RPC handler.
    //
    // TODO:
    // - put some of the configs or services in their own struct?
    #[allow(clippy::too_many_arguments)]
    pub fn new<VersionString, UserAgentString>(
        build_version: VersionString,
        user_agent: UserAgentString,
        network: Network,
        debug_force_finished_sync: bool,
        debug_like_zcashd: bool,
        mempool: Mempool,
        state: State,
        latest_chain_tip: Tip,
    ) -> (Self, JoinHandle<()>)
    where
        VersionString: ToString + Clone + Send + 'static,
        UserAgentString: ToString + Clone + Send + 'static,
    {
        let (runner, queue_sender) = Queue::start();

        let mut build_version = build_version.to_string();
        let user_agent = user_agent.to_string();

        // Match zcashd's version format, if the version string has anything in it
        if !build_version.is_empty() && !build_version.starts_with('v') {
            build_version.insert(0, 'v');
        }

        let rpc_impl = RpcImpl {
            build_version,
            user_agent,
            network: network.clone(),
            debug_force_finished_sync,
            debug_like_zcashd,
            mempool: mempool.clone(),
            state: state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            queue_sender,
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
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    fn get_info(&self) -> Result<GetInfo> {
        let response = GetInfo {
            build: self.build_version.clone(),
            subversion: self.user_agent.clone(),
        };

        Ok(response)
    }

    #[allow(clippy::unwrap_in_result)]
    fn get_blockchain_info(&self) -> BoxFuture<Result<GetBlockChainInfo>> {
        let network = self.network.clone();
        let debug_force_finished_sync = self.debug_force_finished_sync;
        let mut state = self.state.clone();

        async move {
            // `chain` field
            let chain = network.bip70_network_name();

            let request = zebra_state::ReadRequest::TipPoolValues;
            let response: zebra_state::ReadResponse = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_server_error()?;

            let zebra_state::ReadResponse::TipPoolValues {
                tip_height,
                tip_hash,
                value_balance,
            } = response
            else {
                unreachable!("unmatched response to a TipPoolValues request")
            };

            let request = zebra_state::ReadRequest::BlockHeader(tip_hash.into());
            let response: zebra_state::ReadResponse = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_server_error()?;

            let zebra_state::ReadResponse::BlockHeader(block_header) = response else {
                unreachable!("unmatched response to a BlockHeader request")
            };

            let tip_block_time = block_header
                .ok_or_server_error("unexpectedly could not read best chain tip block header")?
                .time;

            let now = Utc::now();
            let zebra_estimated_height =
                NetworkChainTipHeightEstimator::new(tip_block_time, tip_height, &network)
                    .estimate_height_at(now);

            // If we're testing the mempool, force the estimated height to be the actual tip height, otherwise,
            // check if the estimated height is below Zebra's latest tip height, or if the latest tip's block time is
            // later than the current time on the local clock.
            let estimated_height = if tip_block_time > now
                || zebra_estimated_height < tip_height
                || debug_force_finished_sync
            {
                tip_height
            } else {
                zebra_estimated_height
            };

            // `upgrades` object
            //
            // Get the network upgrades in height order, like `zcashd`.
            let mut upgrades = IndexMap::new();
            for (activation_height, network_upgrade) in network.full_activation_list() {
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
                    NetworkUpgrade::current(&network, tip_height)
                        .branch_id()
                        .unwrap_or(ConsensusBranchId::RPC_MISSING_ID),
                ),
                next_block: ConsensusBranchIdHex(
                    NetworkUpgrade::current(&network, next_block_height)
                        .branch_id()
                        .unwrap_or(ConsensusBranchId::RPC_MISSING_ID),
                ),
            };

            let response = GetBlockChainInfo {
                chain,
                blocks: tip_height,
                best_block_hash: tip_hash,
                estimated_height,
                value_pools: types::ValuePoolBalance::from_value_balance(value_balance),
                upgrades,
                consensus,
            };

            Ok(response)
        }
        .boxed()
    }
    fn get_address_balance(
        &self,
        address_strings: AddressStrings,
    ) -> BoxFuture<Result<AddressBalance>> {
        let state = self.state.clone();

        async move {
            let valid_addresses = address_strings.valid_addresses()?;

            let request = zebra_state::ReadRequest::AddressBalance(valid_addresses);
            let response = state.oneshot(request).await.map_server_error()?;

            match response {
                zebra_state::ReadResponse::AddressBalance(balance) => Ok(AddressBalance {
                    balance: u64::from(balance),
                }),
                _ => unreachable!("Unexpected response from state service: {response:?}"),
            }
        }
        .boxed()
    }

    // TODO: use HexData or GetRawTransaction::Bytes to handle the transaction data argument
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
            let _ = queue_sender.send(unmined_transaction);

            let transaction_parameter = mempool::Gossip::Tx(raw_transaction.into());
            let request = mempool::Request::Queue(vec![transaction_parameter]);

            let response = mempool.oneshot(request).await.map_server_error()?;

            let mut queue_results = match response {
                mempool::Response::Queued(results) => results,
                _ => unreachable!("incorrect response variant from mempool service"),
            };

            assert_eq!(
                queue_results.len(),
                1,
                "mempool service returned more results than expected"
            );

            let queue_result = queue_results
                .pop()
                .expect("there should be exactly one item in Vec")
                .inspect_err(|err| tracing::debug!("sent transaction to mempool: {:?}", &err))
                .map_server_error()?
                .await;

            tracing::debug!("sent transaction to mempool: {:?}", &queue_result);

            queue_result
                .map_server_error()?
                .map(|_| SentTransactionHash(transaction_hash))
                .map_server_error()
        }
        .boxed()
    }

    // TODO:
    // - use `height_from_signed_int()` to handle negative heights
    //   (this might be better in the state request, because it needs the state height)
    fn get_block(
        &self,
        hash_or_height: String,
        verbosity: Option<u8>,
    ) -> BoxFuture<Result<GetBlock>> {
        // From <https://zcash.github.io/rpc/getblock.html>
        const DEFAULT_GETBLOCK_VERBOSITY: u8 = 1;

        let mut state = self.state.clone();
        let verbosity = verbosity.unwrap_or(DEFAULT_GETBLOCK_VERBOSITY);

        async move {
            let hash_or_height: HashOrHeight = hash_or_height.parse().map_server_error()?;

            if verbosity == 0 {
                // # Performance
                //
                // This RPC is used in `lightwalletd`'s initial sync of 2 million blocks,
                // so it needs to load block data very efficiently.
                let request = zebra_state::ReadRequest::Block(hash_or_height);
                let response = state
                    .ready()
                    .and_then(|service| service.call(request))
                    .await
                    .map_server_error()?;

                match response {
                    zebra_state::ReadResponse::Block(Some(block)) => {
                        Ok(GetBlock::Raw(block.into()))
                    }
                    zebra_state::ReadResponse::Block(None) => Err(Error {
                        code: MISSING_BLOCK_ERROR_CODE,
                        message: "Block not found".to_string(),
                        data: None,
                    }),
                    _ => unreachable!("unmatched response to a block request"),
                }
            } else if verbosity == 1 || verbosity == 2 {
                // # Performance
                //
                // This RPC is used in `lightwalletd`'s initial sync of 2 million blocks,
                // so it needs to load all its fields very efficiently.
                //
                // Currently, we get the block hash and transaction IDs from indexes,
                // which is much more efficient than loading all the block data,
                // then hashing the block header and all the transactions.

                // Get the block hash from the height -> hash index, if needed
                //
                // # Concurrency
                //
                // For consistency, this lookup must be performed first, then all the other
                // lookups must be based on the hash.
                //
                // All possible responses are valid, even if the best chain changes. Clients
                // must be able to handle chain forks, including a hash for a block that is
                // later discovered to be on a side chain.

                let should_read_block_header = verbosity == 2;

                let hash = match hash_or_height {
                    HashOrHeight::Hash(hash) => hash,
                    HashOrHeight::Height(height) => {
                        let request = zebra_state::ReadRequest::BestChainBlockHash(height);
                        let response = state
                            .ready()
                            .and_then(|service| service.call(request))
                            .await
                            .map_server_error()?;

                        match response {
                            zebra_state::ReadResponse::BlockHash(Some(hash)) => hash,
                            zebra_state::ReadResponse::BlockHash(None) => {
                                return Err(Error {
                                    code: MISSING_BLOCK_ERROR_CODE,
                                    message: "block height not in best chain".to_string(),
                                    data: None,
                                })
                            }
                            _ => unreachable!("unmatched response to a block hash request"),
                        }
                    }
                };

                // TODO:  look up the height if we only have a hash,
                //        this needs a new state request for the height -> hash index
                let height = hash_or_height.height();

                // # Concurrency
                //
                // We look up by block hash so the hash, transaction IDs, and confirmations
                // are consistent.
                let mut requests = vec![
                    // Get transaction IDs from the transaction index by block hash
                    //
                    // # Concurrency
                    //
                    // A block's transaction IDs are never modified, so all possible responses are
                    // valid. Clients that query block heights must be able to handle chain forks,
                    // including getting transaction IDs from any chain fork.
                    zebra_state::ReadRequest::TransactionIdsForBlock(hash.into()),
                    // Sapling trees
                    zebra_state::ReadRequest::SaplingTree(hash.into()),
                    // Orchard trees
                    zebra_state::ReadRequest::OrchardTree(hash.into()),
                    // Get block confirmations from the block height index
                    //
                    // # Concurrency
                    //
                    // All possible responses are valid, even if a block is added to the chain, or
                    // the best chain changes. Clients must be able to handle chain forks, including
                    // different confirmation values before or after added blocks, and switching
                    // between -1 and multiple different confirmation values.
                    zebra_state::ReadRequest::Depth(hash),
                ];

                if should_read_block_header {
                    // Block header
                    requests.push(zebra_state::ReadRequest::BlockHeader(hash.into()))
                }

                let mut futs = FuturesOrdered::new();

                for request in requests {
                    futs.push_back(state.clone().oneshot(request));
                }

                let tx_ids_response = futs.next().await.expect("`futs` should not be empty");
                let tx = match tx_ids_response.map_server_error()? {
                    zebra_state::ReadResponse::TransactionIdsForBlock(tx_ids) => tx_ids
                        .ok_or_server_error("Block not found")?
                        .iter()
                        .map(|tx_id| tx_id.encode_hex())
                        .collect(),
                    _ => unreachable!("unmatched response to a transaction_ids_for_block request"),
                };

                let sapling_tree_response = futs.next().await.expect("`futs` should not be empty");
                let sapling_note_commitment_tree_count =
                    match sapling_tree_response.map_server_error()? {
                        zebra_state::ReadResponse::SaplingTree(Some(nct)) => nct.count(),
                        zebra_state::ReadResponse::SaplingTree(None) => 0,
                        _ => unreachable!("unmatched response to a SaplingTree request"),
                    };

                let orchard_tree_response = futs.next().await.expect("`futs` should not be empty");
                let orchard_note_commitment_tree_count =
                    match orchard_tree_response.map_server_error()? {
                        zebra_state::ReadResponse::OrchardTree(Some(nct)) => nct.count(),
                        zebra_state::ReadResponse::OrchardTree(None) => 0,
                        _ => unreachable!("unmatched response to a OrchardTree request"),
                    };

                // From <https://zcash.github.io/rpc/getblock.html>
                const NOT_IN_BEST_CHAIN_CONFIRMATIONS: i64 = -1;

                let depth_response = futs.next().await.expect("`futs` should not be empty");
                let confirmations = match depth_response.map_server_error()? {
                    // Confirmations are one more than the depth.
                    // Depth is limited by height, so it will never overflow an i64.
                    zebra_state::ReadResponse::Depth(Some(depth)) => i64::from(depth) + 1,
                    zebra_state::ReadResponse::Depth(None) => NOT_IN_BEST_CHAIN_CONFIRMATIONS,
                    _ => unreachable!("unmatched response to a depth request"),
                };

                let time = if should_read_block_header {
                    let block_header_response =
                        futs.next().await.expect("`futs` should not be empty");

                    match block_header_response.map_server_error()? {
                        zebra_state::ReadResponse::BlockHeader(header) => Some(
                            header
                                .ok_or_server_error("Block not found")?
                                .time
                                .timestamp(),
                        ),
                        _ => unreachable!("unmatched response to a BlockHeader request"),
                    }
                } else {
                    None
                };

                let sapling = SaplingTrees {
                    size: sapling_note_commitment_tree_count,
                };

                let orchard = OrchardTrees {
                    size: orchard_note_commitment_tree_count,
                };

                let trees = GetBlockTrees { sapling, orchard };

                Ok(GetBlock::Object {
                    hash: GetBlockHash(hash),
                    confirmations,
                    height,
                    time,
                    tx,
                    trees,
                })
            } else {
                Err(Error {
                    code: ErrorCode::InvalidParams,
                    message: "Invalid verbosity value".to_string(),
                    data: None,
                })
            }
        }
        .boxed()
    }

    fn get_best_block_hash(&self) -> Result<GetBlockHash> {
        self.latest_chain_tip
            .best_tip_hash()
            .map(GetBlockHash)
            .ok_or_server_error("No blocks in state")
    }

    fn get_best_block_height_and_hash(&self) -> Result<GetBlockHeightAndHash> {
        self.latest_chain_tip
            .best_tip_height_and_hash()
            .map(|(height, hash)| GetBlockHeightAndHash { height, hash })
            .ok_or_server_error("No blocks in state")
    }

    fn get_raw_mempool(&self) -> BoxFuture<Result<Vec<String>>> {
        #[cfg(feature = "getblocktemplate-rpcs")]
        use zebra_chain::block::MAX_BLOCK_BYTES;

        #[cfg(feature = "getblocktemplate-rpcs")]
        // Determines whether the output of this RPC is sorted like zcashd
        let should_use_zcashd_order = self.debug_like_zcashd;

        let mut mempool = self.mempool.clone();

        async move {
            #[cfg(feature = "getblocktemplate-rpcs")]
            let request = if should_use_zcashd_order {
                mempool::Request::FullTransactions
            } else {
                mempool::Request::TransactionIds
            };

            #[cfg(not(feature = "getblocktemplate-rpcs"))]
            let request = mempool::Request::TransactionIds;

            // `zcashd` doesn't check if it is synced to the tip here, so we don't either.
            let response = mempool
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_server_error()?;

            match response {
                #[cfg(feature = "getblocktemplate-rpcs")]
                mempool::Response::FullTransactions {
                    mut transactions,
                    last_seen_tip_hash: _,
                } => {
                    // Sort transactions in descending order by fee/size, using hash in serialized byte order as a tie-breaker
                    transactions.sort_by_cached_key(|tx| {
                        // zcashd uses modified fee here but Zebra doesn't currently
                        // support prioritizing transactions
                        std::cmp::Reverse((
                            i64::from(tx.miner_fee) as u128 * MAX_BLOCK_BYTES as u128
                                / tx.transaction.size as u128,
                            // transaction hashes are compared in their serialized byte-order.
                            tx.transaction.id.mined_id(),
                        ))
                    });

                    let tx_ids: Vec<String> = transactions
                        .iter()
                        .map(|unmined_tx| unmined_tx.transaction.id.mined_id().encode_hex())
                        .collect();

                    Ok(tx_ids)
                }

                mempool::Response::TransactionIds(unmined_transaction_ids) => {
                    let mut tx_ids: Vec<String> = unmined_transaction_ids
                        .iter()
                        .map(|id| id.mined_id().encode_hex())
                        .collect();

                    // Sort returned transaction IDs in numeric/string order.
                    tx_ids.sort();

                    Ok(tx_ids)
                }

                _ => unreachable!("unmatched response to a transactionids request"),
            }
        }
        .boxed()
    }

    // TODO: use HexData or SentTransactionHash to handle the transaction ID
    fn get_raw_transaction(
        &self,
        txid_hex: String,
        verbose: Option<u8>,
    ) -> BoxFuture<Result<GetRawTransaction>> {
        let mut state = self.state.clone();
        let mut mempool = self.mempool.clone();
        let verbose = verbose.unwrap_or(0);
        let verbose = verbose != 0;

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
                .map_server_error()?;

            match response {
                mempool::Response::Transactions(unmined_transactions) => {
                    if !unmined_transactions.is_empty() {
                        let tx = unmined_transactions[0].transaction.clone();
                        return Ok(GetRawTransaction::from_transaction(tx, None, 0, verbose));
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
                .map_server_error()?;

            match response {
                zebra_state::ReadResponse::Transaction(Some(MinedTx {
                    tx,
                    height,
                    confirmations,
                })) => Ok(GetRawTransaction::from_transaction(
                    tx,
                    Some(height),
                    confirmations,
                    verbose,
                )),
                zebra_state::ReadResponse::Transaction(None) => {
                    Err("Transaction not found").map_server_error()
                }
                _ => unreachable!("unmatched response to a transaction request"),
            }
        }
        .boxed()
    }

    // TODO:
    // - use `height_from_signed_int()` to handle negative heights
    //   (this might be better in the state request, because it needs the state height)
    fn z_get_treestate(&self, hash_or_height: String) -> BoxFuture<Result<GetTreestate>> {
        let mut state = self.state.clone();
        let network = self.network.clone();

        async move {
            // Convert the [`hash_or_height`] string into an actual hash or height.
            let hash_or_height = hash_or_height.parse().map_server_error()?;

            // Fetch the block referenced by [`hash_or_height`] from the state.
            //
            // # Concurrency
            //
            // For consistency, this lookup must be performed first, then all the other lookups must
            // be based on the hash.
            //
            // TODO: If this RPC is called a lot, just get the block header, rather than the whole block.
            let block = match state
                .ready()
                .and_then(|service| service.call(zebra_state::ReadRequest::Block(hash_or_height)))
                .await
                .map_server_error()?
            {
                zebra_state::ReadResponse::Block(Some(block)) => block,
                zebra_state::ReadResponse::Block(None) => {
                    return Err(Error {
                        code: MISSING_BLOCK_ERROR_CODE,
                        message: "the requested block was not found".to_string(),
                        data: None,
                    })
                }
                _ => unreachable!("unmatched response to a block request"),
            };

            let hash = hash_or_height
                .hash_or_else(|_| Some(block.hash()))
                .expect("block hash");

            let height = hash_or_height
                .height_or_else(|_| block.coinbase_height())
                .expect("verified blocks have a coinbase height");

            let time = u32::try_from(block.header.time.timestamp())
                .expect("Timestamps of valid blocks always fit into u32.");

            let sapling_nu = zcash_primitives::consensus::NetworkUpgrade::Sapling;
            let sapling = if network.is_nu_active(sapling_nu, height.into()) {
                match state
                    .ready()
                    .and_then(|service| {
                        service.call(zebra_state::ReadRequest::SaplingTree(hash.into()))
                    })
                    .await
                    .map_server_error()?
                {
                    zebra_state::ReadResponse::SaplingTree(tree) => tree.map(|t| t.to_rpc_bytes()),
                    _ => unreachable!("unmatched response to a Sapling tree request"),
                }
            } else {
                None
            };

            let orchard_nu = zcash_primitives::consensus::NetworkUpgrade::Nu5;
            let orchard = if network.is_nu_active(orchard_nu, height.into()) {
                match state
                    .ready()
                    .and_then(|service| {
                        service.call(zebra_state::ReadRequest::OrchardTree(hash.into()))
                    })
                    .await
                    .map_server_error()?
                {
                    zebra_state::ReadResponse::OrchardTree(tree) => tree.map(|t| t.to_rpc_bytes()),
                    _ => unreachable!("unmatched response to an Orchard tree request"),
                }
            } else {
                None
            };

            Ok(GetTreestate::from_parts(
                hash, height, time, sapling, orchard,
            ))
        }
        .boxed()
    }

    fn z_get_subtrees_by_index(
        &self,
        pool: String,
        start_index: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> BoxFuture<Result<GetSubtrees>> {
        let mut state = self.state.clone();

        async move {
            const POOL_LIST: &[&str] = &["sapling", "orchard"];

            if pool == "sapling" {
                let request = zebra_state::ReadRequest::SaplingSubtrees { start_index, limit };
                let response = state
                    .ready()
                    .and_then(|service| service.call(request))
                    .await
                    .map_server_error()?;

                let subtrees = match response {
                    zebra_state::ReadResponse::SaplingSubtrees(subtrees) => subtrees,
                    _ => unreachable!("unmatched response to a subtrees request"),
                };

                let subtrees = subtrees
                    .values()
                    .map(|subtree| SubtreeRpcData {
                        root: subtree.root.encode_hex(),
                        end_height: subtree.end_height,
                    })
                    .collect();

                Ok(GetSubtrees {
                    pool,
                    start_index,
                    subtrees,
                })
            } else if pool == "orchard" {
                let request = zebra_state::ReadRequest::OrchardSubtrees { start_index, limit };
                let response = state
                    .ready()
                    .and_then(|service| service.call(request))
                    .await
                    .map_server_error()?;

                let subtrees = match response {
                    zebra_state::ReadResponse::OrchardSubtrees(subtrees) => subtrees,
                    _ => unreachable!("unmatched response to a subtrees request"),
                };

                let subtrees = subtrees
                    .values()
                    .map(|subtree| SubtreeRpcData {
                        root: subtree.root.encode_hex(),
                        end_height: subtree.end_height,
                    })
                    .collect();

                Ok(GetSubtrees {
                    pool,
                    start_index,
                    subtrees,
                })
            } else {
                Err(Error {
                    code: INVALID_PARAMETERS_ERROR_CODE,
                    message: format!("invalid pool name, must be one of: {:?}", POOL_LIST),
                    data: None,
                })
            }
        }
        .boxed()
    }

    fn get_address_tx_ids(
        &self,
        request: GetAddressTxIdsRequest,
    ) -> BoxFuture<Result<Vec<String>>> {
        let mut state = self.state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        let start = Height(request.start);
        let end = Height(request.end);

        async move {
            let chain_height = best_chain_tip_height(&latest_chain_tip)?;

            // height range checks
            check_height_range(start, end, chain_height)?;

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
                .map_server_error()?;

            let hashes = match response {
                zebra_state::ReadResponse::AddressesTransactionIds(hashes) => {
                    let mut last_tx_location = TransactionLocation::from_usize(Height(0), 0);

                    hashes
                        .iter()
                        .map(|(tx_loc, tx_id)| {
                            // Check that the returned transactions are in chain order.
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
                .map_server_error()?;
            let utxos = match response {
                zebra_state::ReadResponse::AddressUtxos(utxos) => utxos,
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
                // Check that the returned UTXOs are in chain order.
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

    // fn get_asset_state(
    //     &self,
    //     asset_base: String,
    //     include_non_finalized: Option<bool>,
    // ) -> BoxFuture<Result<zebra_chain::orchard_zsa::AssetState>> {
    //     let state = self.state.clone();
    //     let include_non_finalized = include_non_finalized.unwrap_or(true);
    //
    //     async move {
    //         let asset_base = hex::decode(asset_base)
    //             .map_server_error()?
    //             .zcash_deserialize_into()
    //             .map_server_error()?;
    //
    //         let request = zebra_state::ReadRequest::AssetState {
    //             asset_base,
    //             include_non_finalized,
    //         };
    //
    //         let zebra_state::ReadResponse::AssetState(asset_state) =
    //             state.oneshot(request).await.map_server_error()?
    //         else {
    //             unreachable!("unexpected response from state service");
    //         };
    //
    //         asset_state.ok_or_server_error("asset base not found")
    //     }
    //     .boxed()
    // }

    fn stop(&self) -> Result<String> {
        #[cfg(not(target_os = "windows"))]
        if self.network.is_regtest() {
            match nix::sys::signal::raise(nix::sys::signal::SIGINT) {
                Ok(_) => Ok("Zebra server stopping".to_string()),
                Err(error) => Err(Error {
                    code: ErrorCode::InternalError,
                    message: format!("Failed to shut down: {}", error),
                    data: None,
                }),
            }
        } else {
            Err(Error {
                code: ErrorCode::MethodNotFound,
                message: "stop is only available on regtest networks".to_string(),
                data: None,
            })
        }
        #[cfg(target_os = "windows")]
        Err(Error {
            code: ErrorCode::MethodNotFound,
            message: "stop is not available in windows targets".to_string(),
            data: None,
        })
    }
}

/// Returns the best chain tip height of `latest_chain_tip`,
/// or an RPC error if there are no blocks in the state.
pub fn best_chain_tip_height<Tip>(latest_chain_tip: &Tip) -> Result<Height>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    latest_chain_tip
        .best_tip_height()
        .ok_or_server_error("No blocks in state")
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

impl Default for GetInfo {
    fn default() -> Self {
        GetInfo {
            build: "some build version".to_string(),
            subversion: "some subversion".to_string(),
        }
    }
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

    /// Value pool balances
    #[serde(rename = "valuePools")]
    value_pools: [types::ValuePoolBalance; 5],

    /// Status of network upgrades
    upgrades: IndexMap<ConsensusBranchIdHex, NetworkUpgradeInfo>,

    /// Branch IDs of the current and upcoming consensus rules
    consensus: TipConsensusBranch,
}

impl Default for GetBlockChainInfo {
    fn default() -> Self {
        GetBlockChainInfo {
            chain: "main".to_string(),
            blocks: Height(1),
            best_block_hash: block::Hash([0; 32]),
            estimated_height: Height(1),
            value_pools: types::ValuePoolBalance::zero_pools(),
            upgrades: IndexMap::new(),
            consensus: TipConsensusBranch {
                chain_tip: ConsensusBranchIdHex(ConsensusBranchId::default()),
                next_block: ConsensusBranchIdHex(ConsensusBranchId::default()),
            },
        }
    }
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
                    Error::invalid_params(format!("invalid address {address:?}: {error}"))
                })
            })
            .collect::<Result<_>>()?;

        Ok(valid_addresses)
    }
}

/// The transparent balance of a set of addresses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize)]
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

impl Default for SentTransactionHash {
    fn default() -> Self {
        Self(transaction::Hash::from([0; 32]))
    }
}

/// Response to a `getblock` RPC request.
///
/// See the notes for the [`Rpc::get_block`] method.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum GetBlock {
    /// The request block, hex-encoded.
    Raw(#[serde(with = "hex")] SerializedBlock),
    /// The block object.
    Object {
        /// The hash of the requested block.
        hash: GetBlockHash,

        /// The number of confirmations of this block in the best chain,
        /// or -1 if it is not in the best chain.
        confirmations: i64,

        /// The height of the requested block.
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<Height>,

        /// The height of the requested block.
        #[serde(skip_serializing_if = "Option::is_none")]
        time: Option<i64>,

        /// List of transaction IDs in block order, hex-encoded.
        //
        // TODO: use a typed Vec<transaction::Hash> here
        tx: Vec<String>,

        /// Information about the note commitment trees.
        trees: GetBlockTrees,
    },
}

impl Default for GetBlock {
    fn default() -> Self {
        GetBlock::Object {
            hash: GetBlockHash::default(),
            confirmations: 0,
            height: None,
            time: None,
            tx: Vec::new(),
            trees: GetBlockTrees::default(),
        }
    }
}

/// Response to a `getbestblockhash` and `getblockhash` RPC request.
///
/// Contains the hex-encoded hash of the requested block.
///
/// Also see the notes for the [`Rpc::get_best_block_hash`] and `get_block_hash` methods.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct GetBlockHash(#[serde(with = "hex")] pub block::Hash);

/// Response to a `getbestblockheightandhash` RPC request.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GetBlockHeightAndHash {
    /// The best chain tip block height
    pub height: block::Height,
    /// The best chain tip block hash
    pub hash: block::Hash,
}

impl Default for GetBlockHeightAndHash {
    fn default() -> Self {
        Self {
            height: block::Height::MIN,
            hash: block::Hash([0; 32]),
        }
    }
}

impl Default for GetBlockHash {
    fn default() -> Self {
        GetBlockHash(block::Hash([0; 32]))
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
        /// The height of the block in the best chain that contains the transaction, or -1 if
        /// the transaction is in the mempool.
        height: i32,
        /// The confirmations of the block in the best chain that contains the transaction,
        /// or 0 if the transaction is in the mempool.
        confirmations: u32,
    },
}

impl Default for GetRawTransaction {
    fn default() -> Self {
        Self::Object {
            hex: SerializedTransaction::from(
                [0u8; zebra_chain::transaction::MIN_TRANSPARENT_TX_SIZE as usize].to_vec(),
            ),
            height: i32::default(),
            confirmations: u32::default(),
        }
    }
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

impl Default for GetAddressUtxos {
    fn default() -> Self {
        Self {
            address: transparent::Address::from_pub_key_hash(
                zebra_chain::parameters::NetworkKind::default(),
                [0u8; 20],
            ),
            txid: transaction::Hash::from([0; 32]),
            output_index: OutputIndex::from_u64(0),
            script: transparent::Script::new(&[0u8; 10]),
            satoshis: u64::default(),
            height: Height(0),
        }
    }
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
        confirmations: u32,
        verbose: bool,
    ) -> Self {
        if verbose {
            GetRawTransaction::Object {
                hex: tx.into(),
                height: match height {
                    Some(height) => height
                        .0
                        .try_into()
                        .expect("valid block heights are limited to i32::MAX"),
                    None => -1,
                },
                confirmations,
            }
        } else {
            GetRawTransaction::Raw(tx.into())
        }
    }
}

/// Information about the sapling and orchard note commitment trees if any.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GetBlockTrees {
    #[serde(skip_serializing_if = "SaplingTrees::is_empty")]
    sapling: SaplingTrees,
    #[serde(skip_serializing_if = "OrchardTrees::is_empty")]
    orchard: OrchardTrees,
}

impl Default for GetBlockTrees {
    fn default() -> Self {
        GetBlockTrees {
            sapling: SaplingTrees { size: 0 },
            orchard: OrchardTrees { size: 0 },
        }
    }
}

/// Sapling note commitment tree information.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SaplingTrees {
    size: u64,
}

impl SaplingTrees {
    fn is_empty(&self) -> bool {
        self.size == 0
    }
}

/// Orchard note commitment tree information.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct OrchardTrees {
    size: u64,
}

impl OrchardTrees {
    fn is_empty(&self) -> bool {
        self.size == 0
    }
}

/// Check if provided height range is valid for address indexes.
fn check_height_range(start: Height, end: Height, chain_height: Height) -> Result<()> {
    if start == Height(0) || end == Height(0) {
        return Err(Error::invalid_params(format!(
            "start {start:?} and end {end:?} must both be greater than zero"
        )));
    }
    if start > end {
        return Err(Error::invalid_params(format!(
            "start {start:?} must be less than or equal to end {end:?}"
        )));
    }
    if start > chain_height || end > chain_height {
        return Err(Error::invalid_params(format!(
            "start {start:?} and end {end:?} must both be less than or equal to the chain tip {chain_height:?}"
        )));
    }

    Ok(())
}

/// Given a potentially negative index, find the corresponding `Height`.
///
/// This function is used to parse the integer index argument of `get_block_hash`.
/// This is based on zcashd's implementation:
/// <https://github.com/zcash/zcash/blob/c267c3ee26510a974554f227d40a89e3ceb5bb4d/src/rpc/blockchain.cpp#L589-L618>
//
// TODO: also use this function in `get_block` and `z_get_treestate`
#[allow(dead_code)]
pub fn height_from_signed_int(index: i32, tip_height: Height) -> Result<Height> {
    if index >= 0 {
        let height = index.try_into().expect("Positive i32 always fits in u32");
        if height > tip_height.0 {
            return Err(Error::invalid_params(
                "Provided index is greater than the current tip",
            ));
        }
        Ok(Height(height))
    } else {
        // `index + 1` can't overflow, because `index` is always negative here.
        let height = i32::try_from(tip_height.0)
            .expect("tip height fits in i32, because Height::MAX fits in i32")
            .checked_add(index + 1);

        let sanitized_height = match height {
            None => return Err(Error::invalid_params("Provided index is not valid")),
            Some(h) => {
                if h < 0 {
                    return Err(Error::invalid_params(
                        "Provided negative index ends up with a negative height",
                    ));
                }
                let h: u32 = h.try_into().expect("Positive i32 always fits in u32");
                if h > tip_height.0 {
                    return Err(Error::invalid_params(
                        "Provided index is greater than the current tip",
                    ));
                }

                h
            }
        };

        Ok(Height(sanitized_height))
    }
}
