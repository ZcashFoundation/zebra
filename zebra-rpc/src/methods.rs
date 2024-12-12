//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `zcashd` server and `lightwalletd` client implementations.

use std::{collections::HashSet, fmt::Debug};

use chrono::Utc;
use futures::{stream::FuturesOrdered, FutureExt, StreamExt, TryFutureExt};
use hex::{FromHex, ToHex};
use hex_data::HexData;
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
    serialization::{ZcashDeserialize, ZcashSerialize},
    subtree::NoteCommitmentSubtreeIndex,
    transaction::{self, SerializedTransaction, Transaction, UnminedTx},
    transparent::{self, Address},
    work::{
        difficulty::{CompactDifficulty, ExpandedDifficulty},
        equihash::Solution,
    },
};
use zebra_node_services::mempool;
use zebra_state::{HashOrHeight, OutputIndex, OutputLocation, TransactionLocation};

use crate::{
    methods::trees::{GetSubtrees, GetTreestate, SubtreeRpcData},
    queue::Queue,
    server::{
        self,
        error::{MapError, OkOrError},
    },
};

pub mod hex_data;

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
    /// [error code `-8`.](https://github.com/zcash/zcash/issues/5758) if a height was
    /// passed or -5 if a hash was passed.
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
    /// Zebra previously partially supported verbosity=1 by returning only the
    /// fields required by lightwalletd ([`lightwalletd` only reads the `tx`
    /// field of the result](https://github.com/zcash/lightwalletd/blob/dfac02093d85fb31fb9a8475b884dd6abca966c7/common/common.go#L152)).
    /// That verbosity level was migrated to "3"; so while lightwalletd will
    /// still work by using verbosity=1, it will sync faster if it is changed to
    /// use verbosity=3.
    ///
    /// The undocumented `chainwork` field is not returned.
    #[rpc(name = "getblock")]
    fn get_block(
        &self,
        hash_or_height: String,
        verbosity: Option<u8>,
    ) -> BoxFuture<Result<GetBlock>>;

    /// Returns the requested block header by hash or height, as a [`GetBlockHeader`] JSON string.
    /// If the block is not in Zebra's state,
    /// returns [error code `-8`.](https://github.com/zcash/zcash/issues/5758)
    /// if a height was passed or -5 if a hash was passed.
    ///
    /// zcashd reference: [`getblockheader`](https://zcash.github.io/rpc/getblockheader.html)
    /// method: post
    /// tags: blockchain
    ///
    /// # Parameters
    ///
    /// - `hash_or_height`: (string, required, example="1") The hash or height for the block to be returned.
    /// - `verbose`: (bool, optional, default=false, example=true) false for hex encoded data, true for a json object
    ///
    /// # Notes
    ///
    /// The undocumented `chainwork` field is not returned.
    #[rpc(name = "getblockheader")]
    fn get_block_header(
        &self,
        hash_or_height: String,
        verbose: Option<bool>,
    ) -> BoxFuture<Result<GetBlockHeader>>;

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
        txid: String,
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
                .map_misc_error()?;

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
                .map_misc_error()?;

            let zebra_state::ReadResponse::BlockHeader { header, .. } = response else {
                unreachable!("unmatched response to a BlockHeader request")
            };

            let tip_block_time = header.time;

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
            let response = state.oneshot(request).await.map_misc_error()?;

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
            // Reference for the legacy error code:
            // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/rawtransaction.cpp#L1259-L1260>
            let raw_transaction_bytes = Vec::from_hex(raw_transaction_hex)
                .map_error(server::error::LegacyCode::Deserialization)?;
            let raw_transaction = Transaction::zcash_deserialize(&*raw_transaction_bytes)
                .map_error(server::error::LegacyCode::Deserialization)?;

            let transaction_hash = raw_transaction.hash();

            // send transaction to the rpc queue, ignore any error.
            let unmined_transaction = UnminedTx::from(raw_transaction.clone());
            let _ = queue_sender.send(unmined_transaction);

            let transaction_parameter = mempool::Gossip::Tx(raw_transaction.into());
            let request = mempool::Request::Queue(vec![transaction_parameter]);

            let response = mempool.oneshot(request).await.map_misc_error()?;

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
                .map_misc_error()?
                .await
                .map_misc_error()?;

            tracing::debug!("sent transaction to mempool: {:?}", &queue_result);

            queue_result
                .map(|_| SentTransactionHash(transaction_hash))
                // Reference for the legacy error code:
                // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/rawtransaction.cpp#L1290-L1301>
                // Note that this error code might not exactly match the one returned by zcashd
                // since zcashd's error code selection logic is more granular. We'd need to
                // propagate the error coming from the verifier to be able to return more specific
                // error codes.
                .map_error(server::error::LegacyCode::Verify)
        }
        .boxed()
    }

    // # Performance
    //
    // `lightwalletd` calls this RPC with verosity 1 for its initial sync of 2 million blocks, the
    // performance of this RPC with verbosity 1 significantly affects `lightwalletd`s sync time.
    //
    // TODO:
    // - use `height_from_signed_int()` to handle negative heights
    //   (this might be better in the state request, because it needs the state height)
    fn get_block(
        &self,
        hash_or_height: String,
        verbosity: Option<u8>,
    ) -> BoxFuture<Result<GetBlock>> {
        let mut state = self.state.clone();
        let verbosity = verbosity.unwrap_or(1);
        let network = self.network.clone();
        let original_hash_or_height = hash_or_height.clone();

        // If verbosity requires a call to `get_block_header`, resolve it here
        let get_block_header_future = if matches!(verbosity, 1 | 2) {
            Some(self.get_block_header(original_hash_or_height.clone(), Some(true)))
        } else {
            None
        };

        async move {
            let hash_or_height: HashOrHeight = hash_or_height
                .parse()
                // Reference for the legacy error code:
                // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
                .map_error(server::error::LegacyCode::InvalidParameter)?;

            if verbosity == 0 {
                let request = zebra_state::ReadRequest::Block(hash_or_height);
                let response = state
                    .ready()
                    .and_then(|service| service.call(request))
                    .await
                    .map_misc_error()?;

                match response {
                    zebra_state::ReadResponse::Block(Some(block)) => {
                        Ok(GetBlock::Raw(block.into()))
                    }
                    zebra_state::ReadResponse::Block(None) => Err("Block not found")
                        .map_error(server::error::LegacyCode::InvalidParameter),
                    _ => unreachable!("unmatched response to a block request"),
                }
            } else if let Some(get_block_header_future) = get_block_header_future {
                let get_block_header_result: Result<GetBlockHeader> = get_block_header_future.await;

                let GetBlockHeader::Object(block_header) = get_block_header_result? else {
                    panic!("must return Object")
                };

                let GetBlockHeaderObject {
                    hash,
                    confirmations,
                    height,
                    version,
                    merkle_root,
                    final_sapling_root,
                    sapling_tree_size,
                    time,
                    nonce,
                    solution,
                    bits,
                    difficulty,
                    previous_block_hash,
                    next_block_hash,
                } = *block_header;

                // # Concurrency
                //
                // We look up by block hash so the hash, transaction IDs, and confirmations
                // are consistent.
                let hash_or_height = hash.0.into();
                let requests = vec![
                    // Get transaction IDs from the transaction index by block hash
                    //
                    // # Concurrency
                    //
                    // A block's transaction IDs are never modified, so all possible responses are
                    // valid. Clients that query block heights must be able to handle chain forks,
                    // including getting transaction IDs from any chain fork.
                    zebra_state::ReadRequest::TransactionIdsForBlock(hash_or_height),
                    // Orchard trees
                    zebra_state::ReadRequest::OrchardTree(hash_or_height),
                ];

                let mut futs = FuturesOrdered::new();

                for request in requests {
                    futs.push_back(state.clone().oneshot(request));
                }

                let tx_ids_response = futs.next().await.expect("`futs` should not be empty");
                let tx = match tx_ids_response.map_misc_error()? {
                    zebra_state::ReadResponse::TransactionIdsForBlock(tx_ids) => tx_ids
                        .ok_or_misc_error("block not found")?
                        .iter()
                        .map(|tx_id| tx_id.encode_hex())
                        .collect(),
                    _ => unreachable!("unmatched response to a transaction_ids_for_block request"),
                };

                let orchard_tree_response = futs.next().await.expect("`futs` should not be empty");
                let zebra_state::ReadResponse::OrchardTree(orchard_tree) =
                    orchard_tree_response.map_misc_error()?
                else {
                    unreachable!("unmatched response to a OrchardTree request");
                };

                let nu5_activation = NetworkUpgrade::Nu5.activation_height(&network);

                // This could be `None` if there's a chain reorg between state queries.
                let orchard_tree = orchard_tree.ok_or_misc_error("missing Orchard tree")?;

                let final_orchard_root = match nu5_activation {
                    Some(activation_height) if height >= activation_height => {
                        Some(orchard_tree.root().into())
                    }
                    _other => None,
                };

                let sapling = SaplingTrees {
                    size: sapling_tree_size,
                };

                let orchard_tree_size = orchard_tree.count();
                let orchard = OrchardTrees {
                    size: orchard_tree_size,
                };

                let trees = GetBlockTrees { sapling, orchard };

                Ok(GetBlock::Object {
                    hash,
                    confirmations,
                    height: Some(height),
                    version: Some(version),
                    merkle_root: Some(merkle_root),
                    time: Some(time),
                    nonce: Some(nonce),
                    solution: Some(solution),
                    bits: Some(bits),
                    difficulty: Some(difficulty),
                    tx,
                    trees,
                    size: None,
                    final_sapling_root: Some(final_sapling_root),
                    final_orchard_root,
                    previous_block_hash: Some(previous_block_hash),
                    next_block_hash,
                })
            } else {
                Err("invalid verbosity value")
                    .map_error(server::error::LegacyCode::InvalidParameter)
            }
        }
        .boxed()
    }

    fn get_block_header(
        &self,
        hash_or_height: String,
        verbose: Option<bool>,
    ) -> BoxFuture<Result<GetBlockHeader>> {
        let state = self.state.clone();
        let verbose = verbose.unwrap_or(true);
        let network = self.network.clone();

        async move {
            let hash_or_height: HashOrHeight = hash_or_height
                .parse()
                .map_error(server::error::LegacyCode::InvalidAddressOrKey)?;
            let zebra_state::ReadResponse::BlockHeader {
                header,
                hash,
                height,
                next_block_hash,
            } = state
                .clone()
                .oneshot(zebra_state::ReadRequest::BlockHeader(hash_or_height))
                .await
                .map_err(|_| "block height not in best chain")
                .map_error(
                    // ## Compatibility with `zcashd`.
                    //
                    // Since this function is reused by getblock(), we return the errors
                    // expected by it (they differ whether a hash or a height was passed).
                    if hash_or_height.hash().is_some() {
                        server::error::LegacyCode::InvalidAddressOrKey
                    } else {
                        server::error::LegacyCode::InvalidParameter
                    },
                )?
            else {
                panic!("unexpected response to BlockHeader request")
            };

            let response = if !verbose {
                GetBlockHeader::Raw(HexData(header.zcash_serialize_to_vec().map_misc_error()?))
            } else {
                let zebra_state::ReadResponse::SaplingTree(sapling_tree) = state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::SaplingTree(hash_or_height))
                    .await
                    .map_misc_error()?
                else {
                    panic!("unexpected response to SaplingTree request")
                };

                // This could be `None` if there's a chain reorg between state queries.
                let sapling_tree = sapling_tree.ok_or_misc_error("missing Sapling tree")?;

                let zebra_state::ReadResponse::Depth(depth) = state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::Depth(hash))
                    .await
                    .map_misc_error()?
                else {
                    panic!("unexpected response to SaplingTree request")
                };

                // From <https://zcash.github.io/rpc/getblock.html>
                // TODO: Deduplicate const definition, consider refactoring this to avoid duplicate logic
                const NOT_IN_BEST_CHAIN_CONFIRMATIONS: i64 = -1;

                // Confirmations are one more than the depth.
                // Depth is limited by height, so it will never overflow an i64.
                let confirmations = depth
                    .map(|depth| i64::from(depth) + 1)
                    .unwrap_or(NOT_IN_BEST_CHAIN_CONFIRMATIONS);

                let mut nonce = *header.nonce;
                nonce.reverse();

                let sapling_activation = NetworkUpgrade::Sapling.activation_height(&network);
                let sapling_tree_size = sapling_tree.count();
                let final_sapling_root: [u8; 32] =
                    if sapling_activation.is_some() && height >= sapling_activation.unwrap() {
                        let mut root: [u8; 32] = sapling_tree.root().into();
                        root.reverse();
                        root
                    } else {
                        [0; 32]
                    };

                let difficulty = header.difficulty_threshold.relative_to_network(&network);

                let block_header = GetBlockHeaderObject {
                    hash: GetBlockHash(hash),
                    confirmations,
                    height,
                    version: header.version,
                    merkle_root: header.merkle_root,
                    final_sapling_root,
                    sapling_tree_size,
                    time: header.time.timestamp(),
                    nonce,
                    solution: header.solution,
                    bits: header.difficulty_threshold,
                    difficulty,
                    previous_block_hash: GetBlockHash(header.previous_block_hash),
                    next_block_hash: next_block_hash.map(GetBlockHash),
                };

                GetBlockHeader::Object(Box::new(block_header))
            };

            Ok(response)
        }
        .boxed()
    }

    fn get_best_block_hash(&self) -> Result<GetBlockHash> {
        self.latest_chain_tip
            .best_tip_hash()
            .map(GetBlockHash)
            .ok_or_misc_error("No blocks in state")
    }

    fn get_best_block_height_and_hash(&self) -> Result<GetBlockHeightAndHash> {
        self.latest_chain_tip
            .best_tip_height_and_hash()
            .map(|(height, hash)| GetBlockHeightAndHash { height, hash })
            .ok_or_misc_error("No blocks in state")
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
                .map_misc_error()?;

            match response {
                #[cfg(feature = "getblocktemplate-rpcs")]
                mempool::Response::FullTransactions {
                    mut transactions,
                    transaction_dependencies: _,
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

    fn get_raw_transaction(
        &self,
        txid: String,
        verbose: Option<u8>,
    ) -> BoxFuture<Result<GetRawTransaction>> {
        let mut state = self.state.clone();
        let mut mempool = self.mempool.clone();
        let verbose = verbose.unwrap_or(0) != 0;

        async move {
            // Reference for the legacy error code:
            // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/rawtransaction.cpp#L544>
            let txid = transaction::Hash::from_hex(txid)
                .map_error(server::error::LegacyCode::InvalidAddressOrKey)?;

            // Check the mempool first.
            match mempool
                .ready()
                .and_then(|service| {
                    service.call(mempool::Request::TransactionsByMinedId([txid].into()))
                })
                .await
                .map_misc_error()?
            {
                mempool::Response::Transactions(txns) => {
                    if let Some(tx) = txns.first() {
                        let hex = tx.transaction.clone().into();

                        return Ok(if verbose {
                            GetRawTransaction::Object {
                                hex,
                                height: None,
                                confirmations: None,
                            }
                        } else {
                            GetRawTransaction::Raw(hex)
                        });
                    }
                }

                _ => unreachable!("unmatched response to a `TransactionsByMinedId` request"),
            };

            // If the tx wasn't in the mempool, check the state.
            match state
                .ready()
                .and_then(|service| service.call(zebra_state::ReadRequest::Transaction(txid)))
                .await
                .map_misc_error()?
            {
                zebra_state::ReadResponse::Transaction(Some(tx)) => {
                    let hex = tx.tx.into();

                    Ok(if verbose {
                        GetRawTransaction::Object {
                            hex,
                            height: Some(tx.height.0),
                            confirmations: Some(tx.confirmations),
                        }
                    } else {
                        GetRawTransaction::Raw(hex)
                    })
                }

                zebra_state::ReadResponse::Transaction(None) => {
                    Err("No such mempool or main chain transaction")
                        .map_error(server::error::LegacyCode::InvalidAddressOrKey)
                }

                _ => unreachable!("unmatched response to a `Transaction` read request"),
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
            // Reference for the legacy error code:
            // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
            let hash_or_height = hash_or_height
                .parse()
                .map_error(server::error::LegacyCode::InvalidParameter)?;

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
                .map_misc_error()?
            {
                zebra_state::ReadResponse::Block(Some(block)) => block,
                zebra_state::ReadResponse::Block(None) => {
                    // Reference for the legacy error code:
                    // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
                    return Err("the requested block is not in the main chain")
                        .map_error(server::error::LegacyCode::InvalidParameter);
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
                    .map_misc_error()?
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
                    .map_misc_error()?
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
                    .map_misc_error()?;

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
                    .map_misc_error()?;

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
                    code: server::error::LegacyCode::Misc.into(),
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
                .map_misc_error()?;

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
                .map_misc_error()?;
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
        .ok_or_misc_error("No blocks in state")
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
        // Reference for the legacy error code:
        // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/misc.cpp#L783-L784>
        let valid_addresses: HashSet<Address> = self
            .addresses
            .into_iter()
            .map(|address| {
                address
                    .parse()
                    .map_error(server::error::LegacyCode::InvalidAddressOrKey)
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
pub struct ConsensusBranchIdHex(#[serde(with = "hex")] ConsensusBranchId);

impl ConsensusBranchIdHex {
    /// Returns a new instance of ['ConsensusBranchIdHex'].
    pub fn new(consensus_branch_id: u32) -> Self {
        ConsensusBranchIdHex(consensus_branch_id.into())
    }

    /// Returns the value of the ['ConsensusBranchId'].
    pub fn inner(&self) -> u32 {
        self.0.into()
    }
}

/// Information about [`NetworkUpgrade`] activation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NetworkUpgradeInfo {
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

impl NetworkUpgradeInfo {
    /// Constructs [`NetworkUpgradeInfo`] from its constituent parts.
    pub fn from_parts(
        name: NetworkUpgrade,
        activation_height: Height,
        status: NetworkUpgradeStatus,
    ) -> Self {
        Self {
            name,
            activation_height,
            status,
        }
    }

    /// Returns the contents of ['NetworkUpgradeInfo'].
    pub fn into_parts(self) -> (NetworkUpgrade, Height, NetworkUpgradeStatus) {
        (self.name, self.activation_height, self.status)
    }
}

/// The activation status of a [`NetworkUpgrade`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NetworkUpgradeStatus {
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
pub struct TipConsensusBranch {
    /// Branch ID used to validate the current chain tip, big-endian, hex-encoded.
    #[serde(rename = "chaintip")]
    chain_tip: ConsensusBranchIdHex,

    /// Branch ID used to validate the next block, big-endian, hex-encoded.
    #[serde(rename = "nextblock")]
    next_block: ConsensusBranchIdHex,
}

impl TipConsensusBranch {
    /// Constructs [`TipConsensusBranch`] from its constituent parts.
    pub fn from_parts(chain_tip: u32, next_block: u32) -> Self {
        Self {
            chain_tip: ConsensusBranchIdHex::new(chain_tip),
            next_block: ConsensusBranchIdHex::new(next_block),
        }
    }

    /// Returns the contents of ['TipConsensusBranch'].
    pub fn into_parts(self) -> (u32, u32) {
        (self.chain_tip.inner(), self.next_block.inner())
    }
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
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] //TODO: create a struct for the Object and Box it
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

        /// The block size. TODO: fill it
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<i64>,

        /// The height of the requested block.
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<Height>,

        /// The version field of the requested block.
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<u32>,

        /// The merkle root of the requested block.
        #[serde(with = "opthex", rename = "merkleroot")]
        #[serde(skip_serializing_if = "Option::is_none")]
        merkle_root: Option<block::merkle::Root>,

        // `blockcommitments` would be here. Undocumented. TODO: decide if we want to support it
        // `authdataroot` would be here. Undocumented. TODO: decide if we want to support it
        //
        /// The root of the Sapling commitment tree after applying this block.
        #[serde(with = "opthex", rename = "finalsaplingroot")]
        #[serde(skip_serializing_if = "Option::is_none")]
        final_sapling_root: Option<[u8; 32]>,

        /// The root of the Orchard commitment tree after applying this block.
        #[serde(with = "opthex", rename = "finalorchardroot")]
        #[serde(skip_serializing_if = "Option::is_none")]
        final_orchard_root: Option<[u8; 32]>,

        // `chainhistoryroot` would be here. Undocumented. TODO: decide if we want to support it
        //
        /// List of transaction IDs in block order, hex-encoded.
        //
        // TODO: use a typed Vec<transaction::Hash> here
        // TODO: support Objects
        tx: Vec<String>,

        /// The height of the requested block.
        #[serde(skip_serializing_if = "Option::is_none")]
        time: Option<i64>,

        /// The nonce of the requested block header.
        #[serde(with = "opthex")]
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<[u8; 32]>,

        /// The Equihash solution in the requested block header.
        /// Note: presence of this field in getblock is not documented in zcashd.
        #[serde(with = "opthex")]
        #[serde(skip_serializing_if = "Option::is_none")]
        solution: Option<Solution>,

        /// The difficulty threshold of the requested block header displayed in compact form.
        #[serde(with = "opthex")]
        #[serde(skip_serializing_if = "Option::is_none")]
        bits: Option<CompactDifficulty>,

        /// Floating point number that represents the difficulty limit for this block as a multiple
        /// of the minimum difficulty for the network.
        #[serde(skip_serializing_if = "Option::is_none")]
        difficulty: Option<f64>,

        // `chainwork` would be here, but we don't plan on supporting it
        // `anchor` would be here. Undocumented. TODO: decide if we want to support it
        // `chainSupply` would be here, TODO: implement
        // `valuePools` would be here, TODO: implement
        //
        /// Information about the note commitment trees.
        trees: GetBlockTrees,

        /// The previous block hash of the requested block header.
        #[serde(rename = "previousblockhash", skip_serializing_if = "Option::is_none")]
        previous_block_hash: Option<GetBlockHash>,

        /// The next block hash after the requested block header.
        #[serde(rename = "nextblockhash", skip_serializing_if = "Option::is_none")]
        next_block_hash: Option<GetBlockHash>,
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
            size: None,
            version: None,
            merkle_root: None,
            final_sapling_root: None,
            final_orchard_root: None,
            nonce: None,
            bits: None,
            difficulty: None,
            previous_block_hash: None,
            next_block_hash: None,
            solution: None,
        }
    }
}

/// Response to a `getblockheader` RPC request.
///
/// See the notes for the [`Rpc::get_block_header`] method.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum GetBlockHeader {
    /// The request block header, hex-encoded.
    Raw(hex_data::HexData),

    /// The block header object.
    Object(Box<GetBlockHeaderObject>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
/// Verbose response to a `getblockheader` RPC request.
///
/// See the notes for the [`Rpc::get_block_header`] method.
pub struct GetBlockHeaderObject {
    /// The hash of the requested block.
    pub hash: GetBlockHash,

    /// The number of confirmations of this block in the best chain,
    /// or -1 if it is not in the best chain.
    pub confirmations: i64,

    /// The height of the requested block.
    pub height: Height,

    /// The version field of the requested block.
    pub version: u32,

    /// The merkle root of the requesteed block.
    #[serde(with = "hex", rename = "merkleroot")]
    pub merkle_root: block::merkle::Root,

    /// The root of the Sapling commitment tree after applying this block.
    #[serde(with = "hex", rename = "finalsaplingroot")]
    pub final_sapling_root: [u8; 32],

    /// The number of Sapling notes in the Sapling note commitment tree
    /// after applying this block. Used by the `getblock` RPC method.
    #[serde(skip)]
    pub sapling_tree_size: u64,

    /// The block time of the requested block header in non-leap seconds since Jan 1 1970 GMT.
    pub time: i64,

    /// The nonce of the requested block header.
    #[serde(with = "hex")]
    pub nonce: [u8; 32],

    /// The Equihash solution in the requested block header.
    #[serde(with = "hex")]
    solution: Solution,

    /// The difficulty threshold of the requested block header displayed in compact form.
    #[serde(with = "hex")]
    pub bits: CompactDifficulty,

    /// Floating point number that represents the difficulty limit for this block as a multiple
    /// of the minimum difficulty for the network.
    pub difficulty: f64,

    /// The previous block hash of the requested block header.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,

    /// The next block hash after the requested block header.
    #[serde(rename = "nextblockhash", skip_serializing_if = "Option::is_none")]
    pub next_block_hash: Option<GetBlockHash>,
}

impl Default for GetBlockHeader {
    fn default() -> Self {
        GetBlockHeader::Object(Box::default())
    }
}

impl Default for GetBlockHeaderObject {
    fn default() -> Self {
        let difficulty: ExpandedDifficulty = zebra_chain::work::difficulty::U256::one().into();

        GetBlockHeaderObject {
            hash: GetBlockHash::default(),
            confirmations: 0,
            height: Height::MIN,
            version: 4,
            merkle_root: block::merkle::Root([0; 32]),
            final_sapling_root: Default::default(),
            sapling_tree_size: Default::default(),
            time: 0,
            nonce: [0; 32],
            solution: Solution::for_proposal(),
            bits: difficulty.to_compact(),
            difficulty: 1.0,
            previous_block_hash: Default::default(),
            next_block_hash: Default::default(),
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
        /// The height of the block in the best chain that contains the tx or `None` if the tx is in
        /// the mempool.
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        /// The height diff between the block containing the tx and the best chain tip + 1 or `None`
        /// if the tx is in the mempool.
        #[serde(skip_serializing_if = "Option::is_none")]
        confirmations: Option<u32>,
    },
}

impl Default for GetRawTransaction {
    fn default() -> Self {
        Self::Object {
            hex: SerializedTransaction::from(
                [0u8; zebra_chain::transaction::MIN_TRANSPARENT_TX_SIZE as usize].to_vec(),
            ),
            height: Option::default(),
            confirmations: Option::default(),
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

impl GetBlockTrees {
    /// Constructs a new instance of ['GetBlockTrees'].
    pub fn new(sapling: u64, orchard: u64) -> Self {
        GetBlockTrees {
            sapling: SaplingTrees { size: sapling },
            orchard: OrchardTrees { size: orchard },
        }
    }

    /// Returns sapling data held by ['GetBlockTrees'].
    pub fn sapling(self) -> u64 {
        self.sapling.size
    }

    /// Returns orchard data held by ['GetBlockTrees'].
    pub fn orchard(self) -> u64 {
        self.orchard.size
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

/// A helper module to serialize `Option<T: ToHex>` as a hex string.
mod opthex {
    use hex::ToHex;
    use serde::Serializer;

    pub fn serialize<S, T>(data: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: ToHex,
    {
        match data {
            Some(data) => {
                let s = data.encode_hex::<String>();
                serializer.serialize_str(&s)
            }
            None => serializer.serialize_none(),
        }
    }
}
