//! Zebra supported RPC methods.
//!
//! Based on the [`zcashd` RPC methods](https://zcash.github.io/rpc/)
//! as used by `lightwalletd.`
//!
//! Some parts of the `zcashd` RPC documentation are outdated.
//! So this implementation follows the `zcashd` server and `lightwalletd` client implementations.

use std::{
    cmp,
    collections::{HashMap, HashSet},
    fmt,
    ops::RangeInclusive,
    sync::Arc,
    time::Duration,
};

use chrono::Utc;
use futures::{future::OptionFuture, stream::FuturesOrdered, StreamExt, TryFutureExt};
use hex::{FromHex, ToHex};
use hex_data::HexData;
use indexmap::IndexMap;
use jsonrpsee::core::{async_trait, RpcResult as Result};
use jsonrpsee_proc_macros::rpc;
use jsonrpsee_types::{ErrorCode, ErrorObject};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zcash_address::{unified::Encoding, TryFromAddress};
use zcash_primitives::consensus::Parameters;

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::{self, Block, Commitment, Height, SerializedBlock, TryIntoHeight},
    chain_sync_status::ChainSyncStatus,
    chain_tip::{ChainTip, NetworkChainTipHeightEstimator},
    parameters::{
        subsidy::{
            block_subsidy, funding_stream_values, miner_subsidy, FundingStreamReceiver,
            ParameterSubsidy,
        },
        ConsensusBranchId, Network, NetworkUpgrade, POW_AVERAGING_WINDOW,
    },
    primitives,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    subtree::NoteCommitmentSubtreeIndex,
    transaction::{self, SerializedTransaction, Transaction, UnminedTx},
    transparent::{self, Address},
    work::{
        difficulty::{CompactDifficulty, ExpandedDifficulty, ParameterDifficulty, U256},
        equihash::Solution,
    },
};
use zebra_consensus::{funding_stream_address, ParameterCheckpoint, RouterError};
use zebra_network::{address_book_peers::AddressBookPeers, PeerSocketAddr};
use zebra_node_services::mempool;
use zebra_state::{
    HashOrHeight, OutputIndex, OutputLocation, ReadRequest, ReadResponse, TransactionLocation,
};

use crate::{
    config,
    methods::trees::{GetSubtrees, GetTreestate, SubtreeRpcData},
    queue::Queue,
    server::{
        self,
        error::{MapError, OkOrError},
    },
};

use types::{
    get_block_template::{
        self, constants::MEMPOOL_LONG_POLL_INTERVAL, proposal::proposal_block_from_template,
        GetBlockTemplate, GetBlockTemplateHandler, ZCASHD_FUNDING_STREAM_ORDER,
    },
    get_blockchain_info, get_mining_info,
    get_raw_mempool::{self, GetRawMempool},
    long_poll::LongPollInput,
    peer_info::PeerInfo,
    submit_block,
    subsidy::BlockSubsidy,
    transaction::TransactionObject,
    unified_address, validate_address, z_validate_address,
};

pub mod hex_data;
pub mod trees;
pub mod types;

#[cfg(test)]
mod tests;

#[rpc(server)]
/// RPC method signatures.
pub trait Rpc {
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
    #[method(name = "getinfo")]
    async fn get_info(&self) -> Result<GetInfo>;

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
    #[method(name = "getblockchaininfo")]
    async fn get_blockchain_info(&self) -> Result<GetBlockChainInfo>;

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
    #[method(name = "getaddressbalance")]
    async fn get_address_balance(&self, address_strings: AddressStrings) -> Result<AddressBalance>;

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
    /// - `allow_high_fees`: (bool, optional) A legacy parameter accepted by zcashd but ignored by Zebra.
    ///
    /// # Notes
    ///
    /// zcashd accepts an optional `allowhighfees` parameter. Zebra doesn't support this parameter,
    /// because lightwalletd doesn't use it.
    #[method(name = "sendrawtransaction")]
    async fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
        _allow_high_fees: Option<bool>,
    ) -> Result<SentTransactionHash>;

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
    /// The `size` field is only returned with verbosity=2.
    ///
    /// The undocumented `chainwork` field is not returned.
    #[method(name = "getblock")]
    async fn get_block(&self, hash_or_height: String, verbosity: Option<u8>) -> Result<GetBlock>;

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
    #[method(name = "getblockheader")]
    async fn get_block_header(
        &self,
        hash_or_height: String,
        verbose: Option<bool>,
    ) -> Result<GetBlockHeader>;

    /// Returns the hash of the current best blockchain tip block, as a [`GetBlockHash`] JSON string.
    ///
    /// zcashd reference: [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)
    /// method: post
    /// tags: blockchain
    #[method(name = "getbestblockhash")]
    fn get_best_block_hash(&self) -> Result<GetBlockHash>;

    /// Returns the height and hash of the current best blockchain tip block, as a [`GetBlockHeightAndHash`] JSON struct.
    ///
    /// zcashd reference: none
    /// method: post
    /// tags: blockchain
    #[method(name = "getbestblockheightandhash")]
    fn get_best_block_height_and_hash(&self) -> Result<GetBlockHeightAndHash>;

    /// Returns all transaction ids in the memory pool, as a JSON array.
    ///
    /// # Parameters
    ///
    /// - `verbose`: (boolean, optional, default=false) true for a json object, false for array of transaction ids.
    ///
    /// zcashd reference: [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)
    /// method: post
    /// tags: blockchain
    #[method(name = "getrawmempool")]
    async fn get_raw_mempool(&self, verbose: Option<bool>) -> Result<GetRawMempool>;

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
    #[method(name = "z_gettreestate")]
    async fn z_get_treestate(&self, hash_or_height: String) -> Result<GetTreestate>;

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
    #[method(name = "z_getsubtreesbyindex")]
    async fn z_get_subtrees_by_index(
        &self,
        pool: String,
        start_index: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<GetSubtrees>;

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
    #[method(name = "getrawtransaction")]
    async fn get_raw_transaction(
        &self,
        txid: String,
        verbose: Option<u8>,
    ) -> Result<GetRawTransaction>;

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
    ///     - `start`: (numeric, optional) The lower height to start looking for transactions (inclusive).
    ///     - `end`: (numeric, optional) The top height to stop looking for transactions (inclusive).
    ///
    /// # Notes
    ///
    /// Only the multi-argument format is used by lightwalletd and this is what we currently support:
    /// <https://github.com/zcash/lightwalletd/blob/631bb16404e3d8b045e74a7c5489db626790b2f6/common/common.go#L97-L102>
    #[method(name = "getaddresstxids")]
    async fn get_address_tx_ids(&self, request: GetAddressTxIdsRequest) -> Result<Vec<String>>;

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
    #[method(name = "getaddressutxos")]
    async fn get_address_utxos(
        &self,
        address_strings: AddressStrings,
    ) -> Result<Vec<GetAddressUtxos>>;

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
    #[method(name = "stop")]
    fn stop(&self) -> Result<String>;

    /// Returns the height of the most recent block in the best valid block chain (equivalently,
    /// the number of blocks in this chain excluding the genesis block).
    ///
    /// zcashd reference: [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)
    /// method: post
    /// tags: blockchain
    #[method(name = "getblockcount")]
    fn get_block_count(&self) -> Result<u32>;

    /// Returns the hash of the block of a given height iff the index argument correspond
    /// to a block in the best chain.
    ///
    /// zcashd reference: [`getblockhash`](https://zcash-rpc.github.io/getblockhash.html)
    /// method: post
    /// tags: blockchain
    ///
    /// # Parameters
    ///
    /// - `index`: (numeric, required, example=1) The block index.
    ///
    /// # Notes
    ///
    /// - If `index` is positive then index = block height.
    /// - If `index` is negative then -1 is the last known valid block.
    #[method(name = "getblockhash")]
    async fn get_block_hash(&self, index: i32) -> Result<GetBlockHash>;

    /// Returns a block template for mining new Zcash blocks.
    ///
    /// # Parameters
    ///
    /// - `jsonrequestobject`: (string, optional) A JSON object containing arguments.
    ///
    /// zcashd reference: [`getblocktemplate`](https://zcash-rpc.github.io/getblocktemplate.html)
    /// method: post
    /// tags: mining
    ///
    /// # Notes
    ///
    /// Arguments to this RPC are currently ignored.
    /// Long polling, block proposals, server lists, and work IDs are not supported.
    ///
    /// Miners can make arbitrary changes to blocks, as long as:
    /// - the data sent to `submitblock` is a valid Zcash block, and
    /// - the parent block is a valid block that Zebra already has, or will receive soon.
    ///
    /// Zebra verifies blocks in parallel, and keeps recent chains in parallel,
    /// so moving between chains and forking chains is very cheap.
    #[method(name = "getblocktemplate")]
    async fn get_block_template(
        &self,
        parameters: Option<get_block_template::parameters::JsonParameters>,
    ) -> Result<get_block_template::Response>;

    /// Submits block to the node to be validated and committed.
    /// Returns the [`submit_block::Response`] for the operation, as a JSON string.
    ///
    /// zcashd reference: [`submitblock`](https://zcash.github.io/rpc/submitblock.html)
    /// method: post
    /// tags: mining
    ///
    /// # Parameters
    ///
    /// - `hexdata`: (string, required)
    /// - `jsonparametersobject`: (string, optional) - currently ignored
    ///
    /// # Notes
    ///
    ///  - `jsonparametersobject` holds a single field, workid, that must be included in submissions if provided by the server.
    #[method(name = "submitblock")]
    async fn submit_block(
        &self,
        hex_data: HexData,
        _parameters: Option<submit_block::JsonParameters>,
    ) -> Result<submit_block::Response>;

    /// Returns mining-related information.
    ///
    /// zcashd reference: [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)
    /// method: post
    /// tags: mining
    #[method(name = "getmininginfo")]
    async fn get_mining_info(&self) -> Result<get_mining_info::Response>;

    /// Returns the estimated network solutions per second based on the last `num_blocks` before
    /// `height`.
    ///
    /// If `num_blocks` is not supplied, uses 120 blocks. If it is 0 or -1, uses the difficulty
    /// averaging window.
    /// If `height` is not supplied or is -1, uses the tip height.
    ///
    /// zcashd reference: [`getnetworksolps`](https://zcash.github.io/rpc/getnetworksolps.html)
    /// method: post
    /// tags: mining
    #[method(name = "getnetworksolps")]
    async fn get_network_sol_ps(&self, num_blocks: Option<i32>, height: Option<i32>)
        -> Result<u64>;

    /// Returns the estimated network solutions per second based on the last `num_blocks` before
    /// `height`.
    ///
    /// This method name is deprecated, use [`getnetworksolps`](Self::get_network_sol_ps) instead.
    /// See that method for details.
    ///
    /// zcashd reference: [`getnetworkhashps`](https://zcash.github.io/rpc/getnetworkhashps.html)
    /// method: post
    /// tags: mining
    #[method(name = "getnetworkhashps")]
    async fn get_network_hash_ps(
        &self,
        num_blocks: Option<i32>,
        height: Option<i32>,
    ) -> Result<u64> {
        self.get_network_sol_ps(num_blocks, height).await
    }

    /// Returns data about each connected network node.
    ///
    /// zcashd reference: [`getpeerinfo`](https://zcash.github.io/rpc/getpeerinfo.html)
    /// method: post
    /// tags: network
    #[method(name = "getpeerinfo")]
    async fn get_peer_info(&self) -> Result<Vec<PeerInfo>>;

    /// Checks if a zcash transparent address of type P2PKH, P2SH or TEX is valid.
    /// Returns information about the given address if valid.
    ///
    /// zcashd reference: [`validateaddress`](https://zcash.github.io/rpc/validateaddress.html)
    /// method: post
    /// tags: util
    ///
    /// # Parameters
    ///
    /// - `address`: (string, required) The zcash address to validate.
    #[method(name = "validateaddress")]
    async fn validate_address(&self, address: String) -> Result<validate_address::Response>;

    /// Checks if a zcash address of type P2PKH, P2SH, TEX, SAPLING or UNIFIED is valid.
    /// Returns information about the given address if valid.
    ///
    /// zcashd reference: [`z_validateaddress`](https://zcash.github.io/rpc/z_validateaddress.html)
    /// method: post
    /// tags: util
    ///
    /// # Parameters
    ///
    /// - `address`: (string, required) The zcash address to validate.
    ///
    /// # Notes
    ///
    /// - No notes
    #[method(name = "z_validateaddress")]
    async fn z_validate_address(&self, address: String) -> Result<z_validate_address::Response>;

    /// Returns the block subsidy reward of the block at `height`, taking into account the mining slow start.
    /// Returns an error if `height` is less than the height of the first halving for the current network.
    ///
    /// zcashd reference: [`getblocksubsidy`](https://zcash.github.io/rpc/getblocksubsidy.html)
    /// method: post
    /// tags: mining
    ///
    /// # Parameters
    ///
    /// - `height`: (numeric, optional, example=1) Can be any valid current or future height.
    ///
    /// # Notes
    ///
    /// If `height` is not supplied, uses the tip height.
    #[method(name = "getblocksubsidy")]
    async fn get_block_subsidy(&self, height: Option<u32>) -> Result<BlockSubsidy>;

    /// Returns the proof-of-work difficulty as a multiple of the minimum difficulty.
    ///
    /// zcashd reference: [`getdifficulty`](https://zcash.github.io/rpc/getdifficulty.html)
    /// method: post
    /// tags: blockchain
    #[method(name = "getdifficulty")]
    async fn get_difficulty(&self) -> Result<f64>;

    /// Returns the list of individual payment addresses given a unified address.
    ///
    /// zcashd reference: [`z_listunifiedreceivers`](https://zcash.github.io/rpc/z_listunifiedreceivers.html)
    /// method: post
    /// tags: wallet
    ///
    /// # Parameters
    ///
    /// - `address`: (string, required) The zcash unified address to get the list from.
    ///
    /// # Notes
    ///
    /// - No notes
    #[method(name = "z_listunifiedreceivers")]
    async fn z_list_unified_receivers(&self, address: String) -> Result<unified_address::Response>;

    /// Invalidates a block if it is not yet finalized, removing it from the non-finalized
    /// state if it is present and rejecting it during contextual validation if it is submitted.
    ///
    /// # Parameters
    ///
    /// - `block_hash`: (hex-encoded block hash, required) The block hash to invalidate.
    // TODO: Invalidate block hashes even if they're not present in the non-finalized state (#9553).
    #[method(name = "invalidateblock")]
    async fn invalidate_block(&self, block_hash: block::Hash) -> Result<()>;

    /// Reconsiders a previously invalidated block if it exists in the cache of previously invalidated blocks.
    ///
    /// # Parameters
    ///
    /// - `block_hash`: (hex-encoded block hash, required) The block hash to reconsider.
    #[method(name = "reconsiderblock")]
    async fn reconsider_block(&self, block_hash: block::Hash) -> Result<Vec<block::Hash>>;

    #[method(name = "generate")]
    /// Mine blocks immediately. Returns the block hashes of the generated blocks.
    ///
    /// # Parameters
    ///
    /// - `num_blocks`: (numeric, required, example=1) Number of blocks to be generated.
    ///
    /// # Notes
    ///
    /// Only works if the network of the running zebrad process is `Regtest`.
    ///
    /// zcashd reference: [`generate`](https://zcash.github.io/rpc/generate.html)
    /// method: post
    /// tags: generating
    async fn generate(&self, num_blocks: u32) -> Result<Vec<GetBlockHash>>;

    #[method(name = "addnode")]
    /// Add or remove a node from the address book.
    ///
    /// # Parameters
    ///
    /// - `addr`: (string, required) The address of the node to add or remove.
    /// - `command`: (string, required) The command to execute, either "add", "onetry", or "remove".
    ///
    /// # Notes
    ///
    /// Only the "add" command is currently supported.
    ///
    /// zcashd reference: [`addnode`](https://zcash.github.io/rpc/addnode.html)
    /// method: post
    /// tags: network
    async fn add_node(&self, addr: PeerSocketAddr, command: AddNodeCommand) -> Result<()>;
}

/// RPC method implementations.
#[derive(Clone)]
pub struct RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>
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
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    ReadState::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
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

    // Services
    //
    /// A handle to the mempool service.
    mempool: Mempool,

    /// A handle to the state service.
    state: State,

    /// A handle to the state service.
    read_state: ReadState,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    // Tasks
    //
    /// A sender component of a channel used to send transactions to the mempool queue.
    queue_sender: broadcast::Sender<UnminedTx>,

    /// Peer address book.
    address_book: AddressBook,

    /// The last warning or error event logged by the server.
    last_warn_error_log_rx: LoggedLastEvent,

    /// Handler for the `getblocktemplate` RPC.
    gbt: GetBlockTemplateHandler<BlockVerifierRouter, SyncStatus>,
}

/// A type alias for the last event logged by the server.
pub type LoggedLastEvent = watch::Receiver<Option<(String, tracing::Level, chrono::DateTime<Utc>)>>;

impl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus> fmt::Debug
    for RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>
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
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    ReadState::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Skip fields without Debug impls, and skip channels
        f.debug_struct("RpcImpl")
            .field("build_version", &self.build_version)
            .field("user_agent", &self.user_agent)
            .field("network", &self.network)
            .field("debug_force_finished_sync", &self.debug_force_finished_sync)
            .field("getblocktemplate", &self.gbt)
            .finish()
    }
}

impl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>
    RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>
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
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    ReadState::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the RPC handler.
    //
    // TODO:
    // - put some of the configs or services in their own struct?
    #[allow(clippy::too_many_arguments)]
    pub fn new<VersionString, UserAgentString>(
        network: Network,
        mining_config: config::mining::Config,
        debug_force_finished_sync: bool,
        build_version: VersionString,
        user_agent: UserAgentString,
        mempool: Mempool,
        state: State,
        read_state: ReadState,
        block_verifier_router: BlockVerifierRouter,
        sync_status: SyncStatus,
        latest_chain_tip: Tip,
        address_book: AddressBook,
        last_warn_error_log_rx: LoggedLastEvent,
        mined_block_sender: Option<watch::Sender<(block::Hash, block::Height)>>,
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

        let gbt = GetBlockTemplateHandler::new(
            &network,
            mining_config.clone(),
            block_verifier_router,
            sync_status,
            mined_block_sender,
        );

        let rpc_impl = RpcImpl {
            build_version,
            user_agent,
            network: network.clone(),
            debug_force_finished_sync,
            mempool: mempool.clone(),
            state: state.clone(),
            read_state: read_state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            queue_sender,
            address_book,
            last_warn_error_log_rx,
            gbt,
        };

        // run the process queue
        let rpc_tx_queue_task_handle = tokio::spawn(
            runner
                .run(mempool, read_state, latest_chain_tip, network)
                .in_current_span(),
        );

        (rpc_impl, rpc_tx_queue_task_handle)
    }
}

#[async_trait]
impl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus> RpcServer
    for RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>
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
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    ReadState::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    async fn get_info(&self) -> Result<GetInfo> {
        let version = GetInfo::version(&self.build_version).expect("invalid version string");

        let connections = self.address_book.recently_live_peers(Utc::now()).len();

        let last_error_recorded = self.last_warn_error_log_rx.borrow().clone();
        let (last_error_log, _level, last_error_log_time) = last_error_recorded.unwrap_or((
            GetInfo::default().errors,
            tracing::Level::INFO,
            Utc::now(),
        ));

        let tip_height = self
            .latest_chain_tip
            .best_tip_height()
            .unwrap_or(Height::MIN);
        let testnet = self.network.is_a_test_network();

        // This field is behind the `ENABLE_WALLET` feature flag in zcashd:
        // https://github.com/zcash/zcash/blob/v6.1.0/src/rpc/misc.cpp#L113
        // However it is not documented as optional:
        // https://github.com/zcash/zcash/blob/v6.1.0/src/rpc/misc.cpp#L70
        // For compatibility, we keep the field in the response, but always return 0.
        let pay_tx_fee = 0.0;

        let relay_fee = zebra_chain::transaction::zip317::MIN_MEMPOOL_TX_FEE_RATE as f64
            / (zebra_chain::amount::COIN as f64);
        let difficulty = chain_tip_difficulty(self.network.clone(), self.read_state.clone(), true)
            .await
            .expect("should always be Ok when `should_use_default` is true");

        let response = GetInfo {
            version,
            build: self.build_version.clone(),
            subversion: self.user_agent.clone(),
            protocol_version: zebra_network::constants::CURRENT_NETWORK_PROTOCOL_VERSION.0,
            blocks: tip_height.0,
            connections,
            proxy: None,
            difficulty,
            testnet,
            pay_tx_fee,
            relay_fee,
            errors: last_error_log,
            errors_timestamp: last_error_log_time.to_string(),
        };

        Ok(response)
    }

    #[allow(clippy::unwrap_in_result)]
    async fn get_blockchain_info(&self) -> Result<GetBlockChainInfo> {
        let debug_force_finished_sync = self.debug_force_finished_sync;
        let network = &self.network;

        let (usage_info_rsp, tip_pool_values_rsp, chain_tip_difficulty) = {
            use zebra_state::ReadRequest::*;
            let state_call = |request| self.read_state.clone().oneshot(request);
            tokio::join!(
                state_call(UsageInfo),
                state_call(TipPoolValues),
                chain_tip_difficulty(network.clone(), self.read_state.clone(), true)
            )
        };

        let (size_on_disk, (tip_height, tip_hash), value_balance, difficulty) = {
            use zebra_state::ReadResponse::*;

            let UsageInfo(size_on_disk) = usage_info_rsp.map_misc_error()? else {
                unreachable!("unmatched response to a TipPoolValues request")
            };

            let (tip, value_balance) = match tip_pool_values_rsp {
                Ok(TipPoolValues {
                    tip_height,
                    tip_hash,
                    value_balance,
                }) => ((tip_height, tip_hash), value_balance),
                Ok(_) => unreachable!("unmatched response to a TipPoolValues request"),
                Err(_) => ((Height::MIN, network.genesis_hash()), Default::default()),
            };

            let difficulty = chain_tip_difficulty
                .expect("should always be Ok when `should_use_default` is true");

            (size_on_disk, tip, value_balance, difficulty)
        };

        let now = Utc::now();
        let (estimated_height, verification_progress) = self
            .latest_chain_tip
            .best_tip_height_and_block_time()
            .map(|(tip_height, tip_block_time)| {
                let height =
                    NetworkChainTipHeightEstimator::new(tip_block_time, tip_height, network)
                        .estimate_height_at(now);

                // If we're testing the mempool, force the estimated height to be the actual tip height, otherwise,
                // check if the estimated height is below Zebra's latest tip height, or if the latest tip's block time is
                // later than the current time on the local clock.
                let height =
                    if tip_block_time > now || height < tip_height || debug_force_finished_sync {
                        tip_height
                    } else {
                        height
                    };

                (height, f64::from(tip_height.0) / f64::from(height.0))
            })
            // TODO: Add a `genesis_block_time()` method on `Network` to use here.
            .unwrap_or((Height::MIN, 0.0));

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
            chain: network.bip70_network_name(),
            blocks: tip_height,
            best_block_hash: tip_hash,
            estimated_height,
            chain_supply: get_blockchain_info::Balance::chain_supply(value_balance),
            value_pools: get_blockchain_info::Balance::value_pools(value_balance),
            upgrades,
            consensus,
            headers: tip_height,
            difficulty,
            verification_progress,
            // TODO: store work in the finalized state for each height (#7109)
            chain_work: 0,
            pruned: false,
            size_on_disk,
            // TODO: Investigate whether this needs to be implemented (it's sprout-only in zcashd)
            commitments: 0,
        };

        Ok(response)
    }

    async fn get_address_balance(&self, address_strings: AddressStrings) -> Result<AddressBalance> {
        let valid_addresses = address_strings.valid_addresses()?;

        let request = zebra_state::ReadRequest::AddressBalance(valid_addresses);
        let response = self
            .read_state
            .clone()
            .oneshot(request)
            .await
            .map_misc_error()?;

        match response {
            zebra_state::ReadResponse::AddressBalance { balance, received } => Ok(AddressBalance {
                balance: u64::from(balance),
                received,
            }),
            _ => unreachable!("Unexpected response from state service: {response:?}"),
        }
    }

    // TODO: use HexData or GetRawTransaction::Bytes to handle the transaction data argument
    async fn send_raw_transaction(
        &self,
        raw_transaction_hex: String,
        _allow_high_fees: Option<bool>,
    ) -> Result<SentTransactionHash> {
        let mempool = self.mempool.clone();
        let queue_sender = self.queue_sender.clone();

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

    // # Performance
    //
    // `lightwalletd` calls this RPC with verosity 1 for its initial sync of 2 million blocks, the
    // performance of this RPC with verbosity 1 significantly affects `lightwalletd`s sync time.
    //
    // TODO:
    // - use `height_from_signed_int()` to handle negative heights
    //   (this might be better in the state request, because it needs the state height)
    async fn get_block(&self, hash_or_height: String, verbosity: Option<u8>) -> Result<GetBlock> {
        let verbosity = verbosity.unwrap_or(1);
        let network = self.network.clone();
        let original_hash_or_height = hash_or_height.clone();

        // If verbosity requires a call to `get_block_header`, resolve it here
        let get_block_header_future = if matches!(verbosity, 1 | 2) {
            Some(self.get_block_header(original_hash_or_height.clone(), Some(true)))
        } else {
            None
        };

        let hash_or_height =
            HashOrHeight::new(&hash_or_height, self.latest_chain_tip.best_tip_height())
                // Reference for the legacy error code:
                // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
                .map_error(server::error::LegacyCode::InvalidParameter)?;

        if verbosity == 0 {
            let request = zebra_state::ReadRequest::Block(hash_or_height);
            let response = self
                .read_state
                .clone()
                .oneshot(request)
                .await
                .map_misc_error()?;

            match response {
                zebra_state::ReadResponse::Block(Some(block)) => Ok(GetBlock::Raw(block.into())),
                zebra_state::ReadResponse::Block(None) => {
                    Err("Block not found").map_error(server::error::LegacyCode::InvalidParameter)
                }
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
                block_commitments,
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

            let transactions_request = match verbosity {
                1 => zebra_state::ReadRequest::TransactionIdsForBlock(hash_or_height),
                2 => zebra_state::ReadRequest::BlockAndSize(hash_or_height),
                _other => panic!("get_block_header_fut should be none"),
            };

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
                transactions_request,
                // Orchard trees
                zebra_state::ReadRequest::OrchardTree(hash_or_height),
            ];

            let mut futs = FuturesOrdered::new();

            for request in requests {
                futs.push_back(self.read_state.clone().oneshot(request));
            }

            let tx_ids_response = futs.next().await.expect("`futs` should not be empty");
            let (tx, size): (Vec<_>, Option<usize>) = match tx_ids_response.map_misc_error()? {
                zebra_state::ReadResponse::TransactionIdsForBlock(tx_ids) => (
                    tx_ids
                        .ok_or_misc_error("block not found")?
                        .iter()
                        .map(|tx_id| GetBlockTransaction::Hash(*tx_id))
                        .collect(),
                    None,
                ),
                zebra_state::ReadResponse::BlockAndSize(block_and_size) => {
                    let (block, size) = block_and_size.ok_or_misc_error("Block not found")?;
                    let block_time = block.header.time;
                    let transactions =
                        block
                            .transactions
                            .iter()
                            .map(|tx| {
                                GetBlockTransaction::Object(Box::new(
                                    TransactionObject::from_transaction(
                                        tx.clone(),
                                        Some(height),
                                        Some(confirmations.try_into().expect(
                                            "should be less than max block height, i32::MAX",
                                        )),
                                        &network,
                                        Some(block_time),
                                    ),
                                ))
                            })
                            .collect();
                    (transactions, Some(size))
                }
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
                size: size.map(|size| size as i64),
                block_commitments: Some(block_commitments),
                final_sapling_root: Some(final_sapling_root),
                final_orchard_root,
                previous_block_hash: Some(previous_block_hash),
                next_block_hash,
            })
        } else {
            Err("invalid verbosity value").map_error(server::error::LegacyCode::InvalidParameter)
        }
    }

    async fn get_block_header(
        &self,
        hash_or_height: String,
        verbose: Option<bool>,
    ) -> Result<GetBlockHeader> {
        let verbose = verbose.unwrap_or(true);
        let network = self.network.clone();

        let hash_or_height =
            HashOrHeight::new(&hash_or_height, self.latest_chain_tip.best_tip_height())
                // Reference for the legacy error code:
                // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
                .map_error(server::error::LegacyCode::InvalidParameter)?;
        let zebra_state::ReadResponse::BlockHeader {
            header,
            hash,
            height,
            next_block_hash,
        } = self
            .read_state
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
            let zebra_state::ReadResponse::SaplingTree(sapling_tree) = self
                .read_state
                .clone()
                .oneshot(zebra_state::ReadRequest::SaplingTree(hash_or_height))
                .await
                .map_misc_error()?
            else {
                panic!("unexpected response to SaplingTree request")
            };

            // This could be `None` if there's a chain reorg between state queries.
            let sapling_tree = sapling_tree.ok_or_misc_error("missing Sapling tree")?;

            let zebra_state::ReadResponse::Depth(depth) = self
                .read_state
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

            let block_commitments = match header.commitment(&network, height).expect(
                "Unexpected failure while parsing the blockcommitments field in get_block_header",
            ) {
                Commitment::PreSaplingReserved(bytes) => bytes,
                Commitment::FinalSaplingRoot(_) => final_sapling_root,
                Commitment::ChainHistoryActivationReserved => [0; 32],
                Commitment::ChainHistoryRoot(root) => root.bytes_in_display_order(),
                Commitment::ChainHistoryBlockTxAuthCommitment(hash) => {
                    hash.bytes_in_display_order()
                }
            };

            let block_header = GetBlockHeaderObject {
                hash: GetBlockHash(hash),
                confirmations,
                height,
                version: header.version,
                merkle_root: header.merkle_root,
                block_commitments,
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

    async fn get_raw_mempool(&self, verbose: Option<bool>) -> Result<GetRawMempool> {
        #[allow(unused)]
        let verbose = verbose.unwrap_or(false);

        use zebra_chain::block::MAX_BLOCK_BYTES;

        let mut mempool = self.mempool.clone();

        let request = if verbose {
            mempool::Request::FullTransactions
        } else {
            mempool::Request::TransactionIds
        };

        // `zcashd` doesn't check if it is synced to the tip here, so we don't either.
        let response = mempool
            .ready()
            .and_then(|service| service.call(request))
            .await
            .map_misc_error()?;

        match response {
            mempool::Response::FullTransactions {
                mut transactions,
                transaction_dependencies,
                last_seen_tip_hash: _,
            } => {
                if verbose {
                    let map = transactions
                        .iter()
                        .map(|unmined_tx| {
                            (
                                unmined_tx.transaction.id.mined_id().encode_hex(),
                                get_raw_mempool::MempoolObject::from_verified_unmined_tx(
                                    unmined_tx,
                                    &transactions,
                                    &transaction_dependencies,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    Ok(GetRawMempool::Verbose(map))
                } else {
                    // Sort transactions in descending order by fee/size, using
                    // hash in serialized byte order as a tie-breaker. Note that
                    // this is only done in not verbose because in verbose mode
                    // a dictionary is returned, where order does not matter.
                    transactions.sort_by_cached_key(|tx| {
                        // zcashd uses modified fee here but Zebra doesn't currently
                        // support prioritizing transactions
                        cmp::Reverse((
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

                    Ok(GetRawMempool::TxIds(tx_ids))
                }
            }

            mempool::Response::TransactionIds(unmined_transaction_ids) => {
                let mut tx_ids: Vec<String> = unmined_transaction_ids
                    .iter()
                    .map(|id| id.mined_id().encode_hex())
                    .collect();

                // Sort returned transaction IDs in numeric/string order.
                tx_ids.sort();

                Ok(GetRawMempool::TxIds(tx_ids))
            }

            _ => unreachable!("unmatched response to a transactionids request"),
        }
    }

    async fn get_raw_transaction(
        &self,
        txid: String,
        verbose: Option<u8>,
    ) -> Result<GetRawTransaction> {
        let mut mempool = self.mempool.clone();
        let verbose = verbose.unwrap_or(0) != 0;

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
                    return Ok(if verbose {
                        GetRawTransaction::Object(Box::new(TransactionObject::from_transaction(
                            tx.transaction.clone(),
                            None,
                            None,
                            &self.network,
                            None,
                        )))
                    } else {
                        let hex = tx.transaction.clone().into();
                        GetRawTransaction::Raw(hex)
                    });
                }
            }

            _ => unreachable!("unmatched response to a `TransactionsByMinedId` request"),
        };

        // If the tx wasn't in the mempool, check the state.
        match self
            .read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Transaction(txid))
            .await
            .map_misc_error()?
        {
            zebra_state::ReadResponse::Transaction(Some(tx)) => Ok(if verbose {
                GetRawTransaction::Object(Box::new(TransactionObject::from_transaction(
                    tx.tx.clone(),
                    Some(tx.height),
                    Some(tx.confirmations),
                    &self.network,
                    // TODO: Performance gain:
                    // https://github.com/ZcashFoundation/zebra/pull/9458#discussion_r2059352752
                    Some(tx.block_time),
                )))
            } else {
                let hex = tx.tx.into();
                GetRawTransaction::Raw(hex)
            }),

            zebra_state::ReadResponse::Transaction(None) => {
                Err("No such mempool or main chain transaction")
                    .map_error(server::error::LegacyCode::InvalidAddressOrKey)
            }

            _ => unreachable!("unmatched response to a `Transaction` read request"),
        }
    }

    // TODO:
    // - use `height_from_signed_int()` to handle negative heights
    //   (this might be better in the state request, because it needs the state height)
    async fn z_get_treestate(&self, hash_or_height: String) -> Result<GetTreestate> {
        let mut read_state = self.read_state.clone();
        let network = self.network.clone();

        let hash_or_height =
            HashOrHeight::new(&hash_or_height, self.latest_chain_tip.best_tip_height())
                // Reference for the legacy error code:
                // <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L629>
                .map_error(server::error::LegacyCode::InvalidParameter)?;

        // Fetch the block referenced by [`hash_or_height`] from the state.
        //
        // # Concurrency
        //
        // For consistency, this lookup must be performed first, then all the other lookups must
        // be based on the hash.
        //
        // TODO: If this RPC is called a lot, just get the block header, rather than the whole block.
        let block = match read_state
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
            match read_state
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
            match read_state
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

    async fn z_get_subtrees_by_index(
        &self,
        pool: String,
        start_index: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<GetSubtrees> {
        let mut read_state = self.read_state.clone();

        const POOL_LIST: &[&str] = &["sapling", "orchard"];

        if pool == "sapling" {
            let request = zebra_state::ReadRequest::SaplingSubtrees { start_index, limit };
            let response = read_state
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
            let response = read_state
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
            Err(ErrorObject::owned(
                server::error::LegacyCode::Misc.into(),
                format!("invalid pool name, must be one of: {:?}", POOL_LIST).as_str(),
                None::<()>,
            ))
        }
    }

    async fn get_address_tx_ids(&self, request: GetAddressTxIdsRequest) -> Result<Vec<String>> {
        let mut read_state = self.read_state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        let height_range = build_height_range(
            request.start,
            request.end,
            best_chain_tip_height(&latest_chain_tip)?,
        )?;

        let valid_addresses = AddressStrings {
            addresses: request.addresses,
        }
        .valid_addresses()?;

        let request = zebra_state::ReadRequest::TransactionIdsByAddresses {
            addresses: valid_addresses,
            height_range,
        };
        let response = read_state
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

    async fn get_address_utxos(
        &self,
        address_strings: AddressStrings,
    ) -> Result<Vec<GetAddressUtxos>> {
        let mut read_state = self.read_state.clone();
        let mut response_utxos = vec![];

        let valid_addresses = address_strings.valid_addresses()?;

        // get utxos data for addresses
        let request = zebra_state::ReadRequest::UtxosByAddresses(valid_addresses);
        let response = read_state
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

    fn stop(&self) -> Result<String> {
        #[cfg(not(target_os = "windows"))]
        if self.network.is_regtest() {
            match nix::sys::signal::raise(nix::sys::signal::SIGINT) {
                Ok(_) => Ok("Zebra server stopping".to_string()),
                Err(error) => Err(ErrorObject::owned(
                    ErrorCode::InternalError.code(),
                    format!("Failed to shut down: {}", error).as_str(),
                    None::<()>,
                )),
            }
        } else {
            Err(ErrorObject::borrowed(
                ErrorCode::MethodNotFound.code(),
                "stop is only available on regtest networks",
                None,
            ))
        }
        #[cfg(target_os = "windows")]
        Err(ErrorObject::borrowed(
            ErrorCode::MethodNotFound.code(),
            "stop is not available in windows targets",
            None,
        ))
    }

    fn get_block_count(&self) -> Result<u32> {
        best_chain_tip_height(&self.latest_chain_tip).map(|height| height.0)
    }

    async fn get_block_hash(&self, index: i32) -> Result<GetBlockHash> {
        let mut read_state = self.read_state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        // TODO: look up this height as part of the state request?
        let tip_height = best_chain_tip_height(&latest_chain_tip)?;

        let height = height_from_signed_int(index, tip_height)?;

        let request = zebra_state::ReadRequest::BestChainBlockHash(height);
        let response = read_state
            .ready()
            .and_then(|service| service.call(request))
            .await
            .map_error(server::error::LegacyCode::default())?;

        match response {
            zebra_state::ReadResponse::BlockHash(Some(hash)) => Ok(GetBlockHash(hash)),
            zebra_state::ReadResponse::BlockHash(None) => Err(ErrorObject::borrowed(
                server::error::LegacyCode::InvalidParameter.into(),
                "Block not found",
                None,
            )),
            _ => unreachable!("unmatched response to a block request"),
        }
    }

    async fn get_block_template(
        &self,
        parameters: Option<get_block_template::JsonParameters>,
    ) -> Result<get_block_template::Response> {
        // Clone Configs
        let network = self.network.clone();
        let extra_coinbase_data = self.gbt.extra_coinbase_data();

        // Clone Services
        let mempool = self.mempool.clone();
        let mut latest_chain_tip = self.latest_chain_tip.clone();
        let sync_status = self.gbt.sync_status();
        let read_state = self.read_state.clone();

        if let Some(HexData(block_proposal_bytes)) = parameters
            .as_ref()
            .and_then(get_block_template::JsonParameters::block_proposal_data)
        {
            return get_block_template::validate_block_proposal(
                self.gbt.block_verifier_router(),
                block_proposal_bytes,
                network,
                latest_chain_tip,
                sync_status,
            )
            .await;
        }

        // To implement long polling correctly, we split this RPC into multiple phases.
        get_block_template::check_parameters(&parameters)?;

        let client_long_poll_id = parameters.as_ref().and_then(|params| params.long_poll_id);

        let miner_address = self
            .gbt
            .miner_address()
            .ok_or_misc_error("miner_address not configured")?;

        // - Checks and fetches that can change during long polling
        //
        // Set up the loop.
        let mut max_time_reached = false;

        // The loop returns the server long poll ID,
        // which should be different to the client long poll ID.
        let (
            server_long_poll_id,
            chain_tip_and_local_time,
            mempool_txs,
            mempool_tx_deps,
            submit_old,
        ) = loop {
            // Check if we are synced to the tip.
            // The result of this check can change during long polling.
            //
            // Optional TODO:
            // - add `async changed()` method to ChainSyncStatus (like `ChainTip`)
            get_block_template::check_synced_to_tip(
                &network,
                latest_chain_tip.clone(),
                sync_status.clone(),
            )?;
            // TODO: return an error if we have no peers, like `zcashd` does,
            //       and add a developer config that mines regardless of how many peers we have.
            // https://github.com/zcash/zcash/blob/6fdd9f1b81d3b228326c9826fa10696fc516444b/src/miner.cpp#L865-L880

            // We're just about to fetch state data, then maybe wait for any changes.
            // Mark all the changes before the fetch as seen.
            // Changes are also ignored in any clones made after the mark.
            latest_chain_tip.mark_best_tip_seen();

            // Fetch the state data and local time for the block template:
            // - if the tip block hash changes, we must return from long polling,
            // - if the local clock changes on testnet, we might return from long polling
            //
            // We always return after 90 minutes on mainnet, even if we have the same response,
            // because the max time has been reached.
            let chain_tip_and_local_time @ zebra_state::GetBlockTemplateChainInfo {
                tip_hash,
                tip_height,
                max_time,
                cur_time,
                ..
            } = get_block_template::fetch_state_tip_and_local_time(read_state.clone()).await?;

            // Fetch the mempool data for the block template:
            // - if the mempool transactions change, we might return from long polling.
            //
            // If the chain fork has just changed, miners want to get the new block as fast
            // as possible, rather than wait for transactions to re-verify. This increases
            // miner profits (and any delays can cause chain forks). So we don't wait between
            // the chain tip changing and getting mempool transactions.
            //
            // Optional TODO:
            // - add a `MempoolChange` type with an `async changed()` method (like `ChainTip`)
            let Some((mempool_txs, mempool_tx_deps)) =
                get_block_template::fetch_mempool_transactions(mempool.clone(), tip_hash)
                    .await?
                    // If the mempool and state responses are out of sync:
                    // - if we are not long polling, omit mempool transactions from the template,
                    // - if we are long polling, continue to the next iteration of the loop to make fresh state and mempool requests.
                    .or_else(|| client_long_poll_id.is_none().then(Default::default))
            else {
                continue;
            };

            // - Long poll ID calculation
            let server_long_poll_id = LongPollInput::new(
                tip_height,
                tip_hash,
                max_time,
                mempool_txs.iter().map(|tx| tx.transaction.id),
            )
            .generate_id();

            // The loop finishes if:
            // - the client didn't pass a long poll ID,
            // - the server long poll ID is different to the client long poll ID, or
            // - the previous loop iteration waited until the max time.
            if Some(&server_long_poll_id) != client_long_poll_id.as_ref() || max_time_reached {
                let mut submit_old = client_long_poll_id
                    .as_ref()
                    .map(|old_long_poll_id| server_long_poll_id.submit_old(old_long_poll_id));

                // On testnet, the max time changes the block difficulty, so old shares are
                // invalid. On mainnet, this means there has been 90 minutes without a new
                // block or mempool transaction, which is very unlikely. So the miner should
                // probably reset anyway.
                if max_time_reached {
                    submit_old = Some(false);
                }

                break (
                    server_long_poll_id,
                    chain_tip_and_local_time,
                    mempool_txs,
                    mempool_tx_deps,
                    submit_old,
                );
            }

            // - Polling wait conditions
            //
            // TODO: when we're happy with this code, split it into a function.
            //
            // Periodically check the mempool for changes.
            //
            // Optional TODO:
            // Remove this polling wait if we switch to using futures to detect sync status
            // and mempool changes.
            let wait_for_mempool_request =
                tokio::time::sleep(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL));

            // Return immediately if the chain tip has changed.
            // The clone preserves the seen status of the chain tip.
            let mut wait_for_best_tip_change = latest_chain_tip.clone();
            let wait_for_best_tip_change = wait_for_best_tip_change.best_tip_changed();

            // Wait for the maximum block time to elapse. This can change the block header
            // on testnet. (On mainnet it can happen due to a network disconnection, or a
            // rapid drop in hash rate.)
            //
            // This duration might be slightly lower than the actual maximum,
            // if cur_time was clamped to min_time. In that case the wait is very long,
            // and it's ok to return early.
            //
            // It can also be zero if cur_time was clamped to max_time. In that case,
            // we want to wait for another change, and ignore this timeout. So we use an
            // `OptionFuture::None`.
            let duration_until_max_time = max_time.saturating_duration_since(cur_time);
            let wait_for_max_time: OptionFuture<_> = if duration_until_max_time.seconds() > 0 {
                Some(tokio::time::sleep(duration_until_max_time.to_std()))
            } else {
                None
            }
            .into();

            // Optional TODO:
            // `zcashd` generates the next coinbase transaction while waiting for changes.
            // When Zebra supports shielded coinbase, we might want to do this in parallel.
            // But the coinbase value depends on the selected transactions, so this needs
            // further analysis to check if it actually saves us any time.

            tokio::select! {
                // Poll the futures in the listed order, for efficiency.
                // We put the most frequent conditions first.
                biased;

                // This timer elapses every few seconds
                _elapsed = wait_for_mempool_request => {
                    tracing::debug!(
                        ?max_time,
                        ?cur_time,
                        ?server_long_poll_id,
                        ?client_long_poll_id,
                        MEMPOOL_LONG_POLL_INTERVAL,
                        "checking for a new mempool change after waiting a few seconds"
                    );
                }

                // The state changes after around a target block interval (75s)
                tip_changed_result = wait_for_best_tip_change => {
                    match tip_changed_result {
                        Ok(()) => {
                            // Spurious updates shouldn't happen in the state, because the
                            // difficulty and hash ordering is a stable total order. But
                            // since they could cause a busy-loop, guard against them here.
                            latest_chain_tip.mark_best_tip_seen();

                            let new_tip_hash = latest_chain_tip.best_tip_hash();
                            if new_tip_hash == Some(tip_hash) {
                                tracing::debug!(
                                    ?max_time,
                                    ?cur_time,
                                    ?server_long_poll_id,
                                    ?client_long_poll_id,
                                    ?tip_hash,
                                    ?tip_height,
                                    "ignoring spurious state change notification"
                                );

                                // Wait for the mempool interval, then check for any changes.
                                tokio::time::sleep(Duration::from_secs(
                                    MEMPOOL_LONG_POLL_INTERVAL,
                                )).await;

                                continue;
                            }

                            tracing::debug!(
                                ?max_time,
                                ?cur_time,
                                ?server_long_poll_id,
                                ?client_long_poll_id,
                                "returning from long poll because state has changed"
                            );
                        }

                        Err(recv_error) => {
                            // This log is rare and helps with debugging, so it's ok to be info.
                            tracing::info!(
                                ?recv_error,
                                ?max_time,
                                ?cur_time,
                                ?server_long_poll_id,
                                ?client_long_poll_id,
                                "returning from long poll due to a state error.\
                                Is Zebra shutting down?"
                            );

                            return Err(recv_error).map_error(server::error::LegacyCode::default());
                        }
                    }
                }

                // The max time does not elapse during normal operation on mainnet,
                // and it rarely elapses on testnet.
                Some(_elapsed) = wait_for_max_time => {
                    // This log is very rare so it's ok to be info.
                    tracing::info!(
                        ?max_time,
                        ?cur_time,
                        ?server_long_poll_id,
                        ?client_long_poll_id,
                        "returning from long poll because max time was reached"
                    );

                    max_time_reached = true;
                }
            }
        };

        // - Processing fetched data to create a transaction template
        //
        // Apart from random weighted transaction selection,
        // the template only depends on the previously fetched data.
        // This processing never fails.

        // Calculate the next block height.
        let next_block_height =
            (chain_tip_and_local_time.tip_height + 1).expect("tip is far below Height::MAX");

        tracing::debug!(
            mempool_tx_hashes = ?mempool_txs
                .iter()
                .map(|tx| tx.transaction.id.mined_id())
                .collect::<Vec<_>>(),
            "selecting transactions for the template from the mempool"
        );

        // Randomly select some mempool transactions.
        let mempool_txs = get_block_template::zip317::select_mempool_transactions(
            &network,
            next_block_height,
            &miner_address,
            mempool_txs,
            mempool_tx_deps,
            extra_coinbase_data.clone(),
        );

        tracing::debug!(
            selected_mempool_tx_hashes = ?mempool_txs
                .iter()
                .map(|#[cfg(not(test))] tx, #[cfg(test)] (_, tx)| tx.transaction.id.mined_id())
                .collect::<Vec<_>>(),
            "selected transactions for the template from the mempool"
        );

        // - After this point, the template only depends on the previously fetched data.

        let response = GetBlockTemplate::new(
            &network,
            &miner_address,
            &chain_tip_and_local_time,
            server_long_poll_id,
            mempool_txs,
            submit_old,
            extra_coinbase_data,
        );

        Ok(response.into())
    }

    async fn submit_block(
        &self,
        HexData(block_bytes): HexData,
        _parameters: Option<submit_block::JsonParameters>,
    ) -> Result<submit_block::Response> {
        let mut block_verifier_router = self.gbt.block_verifier_router();

        let block: Block = match block_bytes.zcash_deserialize_into() {
            Ok(block_bytes) => block_bytes,
            Err(error) => {
                tracing::info!(
                    ?error,
                    "submit block failed: block bytes could not be deserialized into a structurally valid block"
                );

                return Ok(submit_block::ErrorResponse::Rejected.into());
            }
        };

        let height = block
            .coinbase_height()
            .ok_or_error(0, "coinbase height not found")?;
        let block_hash = block.hash();

        let block_verifier_router_response = block_verifier_router
            .ready()
            .await
            .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?
            .call(zebra_consensus::Request::Commit(Arc::new(block)))
            .await;

        let chain_error = match block_verifier_router_response {
            // Currently, this match arm returns `null` (Accepted) for blocks committed
            // to any chain, but Accepted is only for blocks in the best chain.
            //
            // TODO (#5487):
            // - Inconclusive: check if the block is on a side-chain
            // The difference is important to miners, because they want to mine on the best chain.
            Ok(hash) => {
                tracing::info!(?hash, ?height, "submit block accepted");

                self.gbt
                    .advertise_mined_block(hash, height)
                    .map_error_with_prefix(0, "failed to send mined block")?;

                return Ok(submit_block::Response::Accepted);
            }

            // Turns BoxError into Result<VerifyChainError, BoxError>,
            // by downcasting from Any to VerifyChainError.
            Err(box_error) => {
                let error = box_error
                    .downcast::<RouterError>()
                    .map(|boxed_chain_error| *boxed_chain_error);

                tracing::info!(
                    ?error,
                    ?block_hash,
                    ?height,
                    "submit block failed verification"
                );

                error
            }
        };

        let response = match chain_error {
            Ok(source) if source.is_duplicate_request() => submit_block::ErrorResponse::Duplicate,

            // Currently, these match arms return Reject for the older duplicate in a queue,
            // but queued duplicates should be DuplicateInconclusive.
            //
            // Optional TODO (#5487):
            // - DuplicateInconclusive: turn these non-finalized state duplicate block errors
            //   into BlockError enum variants, and handle them as DuplicateInconclusive:
            //   - "block already sent to be committed to the state"
            //   - "replaced by newer request"
            // - keep the older request in the queue,
            //   and return a duplicate error for the newer request immediately.
            //   This improves the speed of the RPC response.
            //
            // Checking the download queues and BlockVerifierRouter buffer for duplicates
            // might require architectural changes to Zebra, so we should only do it
            // if mining pools really need it.
            Ok(_verify_chain_error) => submit_block::ErrorResponse::Rejected,

            // This match arm is currently unreachable, but if future changes add extra error types,
            // we want to turn them into `Rejected`.
            Err(_unknown_error_type) => submit_block::ErrorResponse::Rejected,
        };

        Ok(response.into())
    }

    async fn get_mining_info(&self) -> Result<get_mining_info::Response> {
        let network = self.network.clone();
        let mut read_state = self.read_state.clone();

        let chain_tip = self.latest_chain_tip.clone();
        let tip_height = chain_tip.best_tip_height().unwrap_or(Height(0)).0;

        let mut current_block_tx = None;
        if tip_height > 0 {
            let mined_tx_ids = chain_tip.best_tip_mined_transaction_ids();
            current_block_tx =
                (!mined_tx_ids.is_empty()).then(|| mined_tx_ids.len().saturating_sub(1));
        }

        let solution_rate_fut = self.get_network_sol_ps(None, None);
        // Get the current block size.
        let mut current_block_size = None;
        if tip_height > 0 {
            let request = zebra_state::ReadRequest::TipBlockSize;
            let response: zebra_state::ReadResponse = read_state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_error(server::error::LegacyCode::default())?;
            current_block_size = match response {
                zebra_state::ReadResponse::TipBlockSize(Some(block_size)) => Some(block_size),
                _ => None,
            };
        }

        Ok(get_mining_info::Response::new(
            tip_height,
            current_block_size,
            current_block_tx,
            network,
            solution_rate_fut.await?,
        ))
    }

    async fn get_network_sol_ps(
        &self,
        num_blocks: Option<i32>,
        height: Option<i32>,
    ) -> Result<u64> {
        // Default number of blocks is 120 if not supplied.
        let mut num_blocks =
            num_blocks.unwrap_or(get_block_template::DEFAULT_SOLUTION_RATE_WINDOW_SIZE);
        // But if it is 0 or negative, it uses the proof of work averaging window.
        if num_blocks < 1 {
            num_blocks = i32::try_from(POW_AVERAGING_WINDOW).expect("fits in i32");
        }
        let num_blocks =
            usize::try_from(num_blocks).expect("just checked for negatives, i32 fits in usize");

        // Default height is the tip height if not supplied. Negative values also mean the tip
        // height. Since negative values aren't valid heights, we can just use the conversion.
        let height = height.and_then(|height| height.try_into_height().ok());

        let mut read_state = self.read_state.clone();

        let request = ReadRequest::SolutionRate { num_blocks, height };

        let response = read_state
            .ready()
            .and_then(|service| service.call(request))
            .await
            .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?;

        let solution_rate = match response {
            // zcashd returns a 0 rate when the calculation is invalid
            ReadResponse::SolutionRate(solution_rate) => solution_rate.unwrap_or(0),

            _ => unreachable!("unmatched response to a solution rate request"),
        };

        Ok(solution_rate
            .try_into()
            .expect("per-second solution rate always fits in u64"))
    }

    async fn get_peer_info(&self) -> Result<Vec<PeerInfo>> {
        let address_book = self.address_book.clone();
        Ok(address_book
            .recently_live_peers(chrono::Utc::now())
            .into_iter()
            .map(PeerInfo::from)
            .collect())
    }

    async fn validate_address(&self, raw_address: String) -> Result<validate_address::Response> {
        let network = self.network.clone();

        let Ok(address) = raw_address.parse::<zcash_address::ZcashAddress>() else {
            return Ok(validate_address::Response::invalid());
        };

        let address = match address.convert::<primitives::Address>() {
            Ok(address) => address,
            Err(err) => {
                tracing::debug!(?err, "conversion error");
                return Ok(validate_address::Response::invalid());
            }
        };

        // we want to match zcashd's behaviour
        if !address.is_transparent() {
            return Ok(validate_address::Response::invalid());
        }

        if address.network() == network.kind() {
            Ok(validate_address::Response {
                address: Some(raw_address),
                is_valid: true,
                is_script: Some(address.is_script_hash()),
            })
        } else {
            tracing::info!(
                ?network,
                address_network = ?address.network(),
                "invalid address in validateaddress RPC: Zebra's configured network must match address network"
            );

            Ok(validate_address::Response::invalid())
        }
    }

    async fn z_validate_address(
        &self,
        raw_address: String,
    ) -> Result<z_validate_address::Response> {
        let network = self.network.clone();

        let Ok(address) = raw_address.parse::<zcash_address::ZcashAddress>() else {
            return Ok(z_validate_address::Response::invalid());
        };

        let address = match address.convert::<primitives::Address>() {
            Ok(address) => address,
            Err(err) => {
                tracing::debug!(?err, "conversion error");
                return Ok(z_validate_address::Response::invalid());
            }
        };

        if address.network() == network.kind() {
            Ok(z_validate_address::Response {
                is_valid: true,
                address: Some(raw_address),
                address_type: Some(z_validate_address::AddressType::from(&address)),
                is_mine: Some(false),
            })
        } else {
            tracing::info!(
                ?network,
                address_network = ?address.network(),
                "invalid address network in z_validateaddress RPC: address is for {:?} but Zebra is on {:?}",
                address.network(),
                network
            );

            Ok(z_validate_address::Response::invalid())
        }
    }

    async fn get_block_subsidy(&self, height: Option<u32>) -> Result<BlockSubsidy> {
        let latest_chain_tip = self.latest_chain_tip.clone();
        let network = self.network.clone();

        let height = if let Some(height) = height {
            Height(height)
        } else {
            best_chain_tip_height(&latest_chain_tip)?
        };

        if height < network.height_for_first_halving() {
            return Err(ErrorObject::borrowed(
                0,
                "Zebra does not support founders' reward subsidies, \
                        use a block height that is after the first halving",
                None,
            ));
        }

        // Always zero for post-halving blocks
        let founders = Amount::zero();

        let total_block_subsidy =
            block_subsidy(height, &network).map_error(server::error::LegacyCode::default())?;
        let miner_subsidy = miner_subsidy(height, &network, total_block_subsidy)
            .map_error(server::error::LegacyCode::default())?;

        let (lockbox_streams, mut funding_streams): (Vec<_>, Vec<_>) =
            funding_stream_values(height, &network, total_block_subsidy)
                .map_error(server::error::LegacyCode::default())?
                .into_iter()
                // Separate the funding streams into deferred and non-deferred streams
                .partition(|(receiver, _)| matches!(receiver, FundingStreamReceiver::Deferred));

        let is_nu6 = NetworkUpgrade::current(&network, height) == NetworkUpgrade::Nu6;

        let [lockbox_total, funding_streams_total]: [std::result::Result<
            Amount<NonNegative>,
            amount::Error,
        >; 2] = [&lockbox_streams, &funding_streams]
            .map(|streams| streams.iter().map(|&(_, amount)| amount).sum());

        // Use the same funding stream order as zcashd
        funding_streams.sort_by_key(|(receiver, _funding_stream)| {
            ZCASHD_FUNDING_STREAM_ORDER
                .iter()
                .position(|zcashd_receiver| zcashd_receiver == receiver)
        });

        // Format the funding streams and lockbox streams
        let [funding_streams, lockbox_streams]: [Vec<_>; 2] = [funding_streams, lockbox_streams]
            .map(|streams| {
                streams
                    .into_iter()
                    .map(|(receiver, value)| {
                        let address = funding_stream_address(height, &network, receiver);
                        types::subsidy::FundingStream::new(is_nu6, receiver, value, address)
                    })
                    .collect()
            });

        Ok(BlockSubsidy {
            miner: miner_subsidy.into(),
            founders: founders.into(),
            funding_streams,
            lockbox_streams,
            funding_streams_total: funding_streams_total
                .map_error(server::error::LegacyCode::default())?
                .into(),
            lockbox_total: lockbox_total
                .map_error(server::error::LegacyCode::default())?
                .into(),
            total_block_subsidy: total_block_subsidy.into(),
        })
    }

    async fn get_difficulty(&self) -> Result<f64> {
        chain_tip_difficulty(self.network.clone(), self.read_state.clone(), false).await
    }

    async fn z_list_unified_receivers(&self, address: String) -> Result<unified_address::Response> {
        use zcash_address::unified::Container;

        let (network, unified_address): (
            zcash_protocol::consensus::NetworkType,
            zcash_address::unified::Address,
        ) = zcash_address::unified::Encoding::decode(address.clone().as_str())
            .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?;

        let mut p2pkh = String::new();
        let mut p2sh = String::new();
        let mut orchard = String::new();
        let mut sapling = String::new();

        for item in unified_address.items() {
            match item {
                zcash_address::unified::Receiver::Orchard(_data) => {
                    let addr = zcash_address::unified::Address::try_from_items(vec![item])
                        .expect("using data already decoded as valid");
                    orchard = addr.encode(&network);
                }
                zcash_address::unified::Receiver::Sapling(data) => {
                    let addr = zebra_chain::primitives::Address::try_from_sapling(network, data)
                        .expect("using data already decoded as valid");
                    sapling = addr.payment_address().unwrap_or_default();
                }
                zcash_address::unified::Receiver::P2pkh(data) => {
                    let addr =
                        zebra_chain::primitives::Address::try_from_transparent_p2pkh(network, data)
                            .expect("using data already decoded as valid");
                    p2pkh = addr.payment_address().unwrap_or_default();
                }
                zcash_address::unified::Receiver::P2sh(data) => {
                    let addr =
                        zebra_chain::primitives::Address::try_from_transparent_p2sh(network, data)
                            .expect("using data already decoded as valid");
                    p2sh = addr.payment_address().unwrap_or_default();
                }
                _ => (),
            }
        }

        Ok(unified_address::Response::new(
            orchard, sapling, p2pkh, p2sh,
        ))
    }

    async fn invalidate_block(&self, block_hash: block::Hash) -> Result<()> {
        self.state
            .clone()
            .oneshot(zebra_state::Request::InvalidateBlock(block_hash))
            .await
            .map(|rsp| assert_eq!(rsp, zebra_state::Response::Invalidated(block_hash)))
            .map_misc_error()
    }

    async fn reconsider_block(&self, block_hash: block::Hash) -> Result<Vec<block::Hash>> {
        self.state
            .clone()
            .oneshot(zebra_state::Request::ReconsiderBlock(block_hash))
            .await
            .map(|rsp| match rsp {
                zebra_state::Response::Reconsidered(block_hashes) => block_hashes,
                _ => unreachable!("unmatched response to a reconsider block request"),
            })
            .map_misc_error()
    }

    async fn generate(&self, num_blocks: u32) -> Result<Vec<GetBlockHash>> {
        let rpc = self.clone();
        let network = self.network.clone();

        if !network.disable_pow() {
            return Err(ErrorObject::borrowed(
                0,
                "generate is only supported on networks where PoW is disabled",
                None,
            ));
        }

        let mut block_hashes = Vec::new();
        for _ in 0..num_blocks {
            let block_template = rpc
                .get_block_template(None)
                .await
                .map_error(server::error::LegacyCode::default())?;

            let get_block_template::Response::TemplateMode(block_template) = block_template else {
                return Err(ErrorObject::borrowed(
                    0,
                    "error generating block template",
                    None,
                ));
            };

            let proposal_block = proposal_block_from_template(
                &block_template,
                get_block_template::TimeSource::CurTime,
            )
            .map_error(server::error::LegacyCode::default())?;
            let hex_proposal_block = HexData(
                proposal_block
                    .zcash_serialize_to_vec()
                    .map_error(server::error::LegacyCode::default())?,
            );

            let _submit = rpc
                .submit_block(hex_proposal_block, None)
                .await
                .map_error(server::error::LegacyCode::default())?;

            block_hashes.push(GetBlockHash(proposal_block.hash()));
        }

        Ok(block_hashes)
    }

    async fn add_node(
        &self,
        addr: zebra_network::PeerSocketAddr,
        command: AddNodeCommand,
    ) -> Result<()> {
        if self.network.is_regtest() {
            match command {
                AddNodeCommand::Add => {
                    tracing::info!(?addr, "adding peer address to the address book");
                    if self.address_book.clone().add_peer(addr) {
                        Ok(())
                    } else {
                        return Err(ErrorObject::owned(
                            ErrorCode::InvalidParams.code(),
                            format!("peer address was already present in the address book: {addr}"),
                            None::<()>,
                        ));
                    }
                }
            }
        } else {
            return Err(ErrorObject::owned(
                ErrorCode::InvalidParams.code(),
                "addnode command is only supported on regtest",
                None::<()>,
            ));
        }
    }
}

// TODO: Move the code below to separate modules.

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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetInfo {
    /// The node version
    version: u64,

    /// The node version build number
    build: String,

    /// The server sub-version identifier, used as the network protocol user-agent
    subversion: String,

    /// The protocol version
    #[serde(rename = "protocolversion")]
    protocol_version: u32,

    /// The current number of blocks processed in the server
    blocks: u32,

    /// The total (inbound and outbound) number of connections the node has
    connections: usize,

    /// The proxy (if any) used by the server. Currently always `None` in Zebra.
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<String>,

    /// The current network difficulty
    difficulty: f64,

    /// True if the server is running in testnet mode, false otherwise
    testnet: bool,

    /// The minimum transaction fee in ZEC/kB
    #[serde(rename = "paytxfee")]
    pay_tx_fee: f64,

    /// The minimum relay fee for non-free transactions in ZEC/kB
    #[serde(rename = "relayfee")]
    relay_fee: f64,

    /// The last error or warning message, or "no errors" if there are no errors
    errors: String,

    /// The time of the last error or warning message, or "no errors timestamp" if there are no errors
    #[serde(rename = "errorstimestamp")]
    errors_timestamp: String,
}

impl Default for GetInfo {
    fn default() -> Self {
        GetInfo {
            version: 0,
            build: "some build version".to_string(),
            subversion: "some subversion".to_string(),
            protocol_version: 0,
            blocks: 0,
            connections: 0,
            proxy: None,
            difficulty: 0.0,
            testnet: false,
            pay_tx_fee: 0.0,
            relay_fee: 0.0,
            errors: "no errors".to_string(),
            errors_timestamp: "no errors timestamp".to_string(),
        }
    }
}

impl GetInfo {
    /// Constructs [`GetInfo`] from its constituent parts.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        version: u64,
        build: String,
        subversion: String,
        protocol_version: u32,
        blocks: u32,
        connections: usize,
        proxy: Option<String>,
        difficulty: f64,
        testnet: bool,
        pay_tx_fee: f64,
        relay_fee: f64,
        errors: String,
        errors_timestamp: String,
    ) -> Self {
        Self {
            version,
            build,
            subversion,
            protocol_version,
            blocks,
            connections,
            proxy,
            difficulty,
            testnet,
            pay_tx_fee,
            relay_fee,
            errors,
            errors_timestamp,
        }
    }

    /// Returns the contents of ['GetInfo'].
    pub fn into_parts(
        self,
    ) -> (
        u64,
        String,
        String,
        u32,
        u32,
        usize,
        Option<String>,
        f64,
        bool,
        f64,
        f64,
        String,
        String,
    ) {
        (
            self.version,
            self.build,
            self.subversion,
            self.protocol_version,
            self.blocks,
            self.connections,
            self.proxy,
            self.difficulty,
            self.testnet,
            self.pay_tx_fee,
            self.relay_fee,
            self.errors,
            self.errors_timestamp,
        )
    }

    /// Create the node version number.
    pub fn version(build_string: &str) -> Option<u64> {
        let semver_version = semver::Version::parse(build_string.strip_prefix('v')?).ok()?;
        let build_number = semver_version
            .build
            .as_str()
            .split('.')
            .next()
            .and_then(|num_str| num_str.parse::<u64>().ok())
            .unwrap_or_default();

        // https://github.com/zcash/zcash/blob/v6.1.0/src/clientversion.h#L55-L59
        let version_number = 1_000_000 * semver_version.major
            + 10_000 * semver_version.minor
            + 100 * semver_version.patch
            + build_number;

        Some(version_number)
    }
}

/// Response to a `getblockchaininfo` RPC request.
///
/// See the notes for the [`Rpc::get_blockchain_info` method].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GetBlockChainInfo {
    /// Current network name as defined in BIP70 (main, test, regtest)
    chain: String,

    /// The current number of blocks processed in the server, numeric
    blocks: Height,

    /// The current number of headers we have validated in the best chain, that is,
    /// the height of the best chain.
    headers: Height,

    /// The estimated network solution rate in Sol/s.
    difficulty: f64,

    /// The verification progress relative to the estimated network chain tip.
    #[serde(rename = "verificationprogress")]
    verification_progress: f64,

    /// The total amount of work in the best chain, hex-encoded.
    #[serde(rename = "chainwork")]
    chain_work: u64,

    /// Whether this node is pruned, currently always false in Zebra.
    pruned: bool,

    /// The estimated size of the block and undo files on disk
    size_on_disk: u64,

    /// The current number of note commitments in the commitment tree
    commitments: u64,

    /// The hash of the currently best block, in big-endian order, hex-encoded
    #[serde(rename = "bestblockhash", with = "hex")]
    best_block_hash: block::Hash,

    /// If syncing, the estimated height of the chain, else the current best height, numeric.
    ///
    /// In Zebra, this is always the height estimate, so it might be a little inaccurate.
    #[serde(rename = "estimatedheight")]
    estimated_height: Height,

    /// Chain supply balance
    #[serde(rename = "chainSupply")]
    chain_supply: get_blockchain_info::Balance,

    /// Value pool balances
    #[serde(rename = "valuePools")]
    value_pools: [get_blockchain_info::Balance; 5],

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
            chain_supply: get_blockchain_info::Balance::chain_supply(Default::default()),
            value_pools: get_blockchain_info::Balance::zero_pools(),
            upgrades: IndexMap::new(),
            consensus: TipConsensusBranch {
                chain_tip: ConsensusBranchIdHex(ConsensusBranchId::default()),
                next_block: ConsensusBranchIdHex(ConsensusBranchId::default()),
            },
            headers: Height(1),
            difficulty: 0.0,
            verification_progress: 0.0,
            chain_work: 0,
            pruned: false,
            size_on_disk: 0,
            commitments: 0,
        }
    }
}

impl GetBlockChainInfo {
    /// Creates a new [`GetBlockChainInfo`] instance.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain: String,
        blocks: Height,
        best_block_hash: block::Hash,
        estimated_height: Height,
        chain_supply: get_blockchain_info::Balance,
        value_pools: [get_blockchain_info::Balance; 5],
        upgrades: IndexMap<ConsensusBranchIdHex, NetworkUpgradeInfo>,
        consensus: TipConsensusBranch,
        headers: Height,
        difficulty: f64,
        verification_progress: f64,
        chain_work: u64,
        pruned: bool,
        size_on_disk: u64,
        commitments: u64,
    ) -> Self {
        Self {
            chain,
            blocks,
            best_block_hash,
            estimated_height,
            chain_supply,
            value_pools,
            upgrades,
            consensus,
            headers,
            difficulty,
            verification_progress,
            chain_work,
            pruned,
            size_on_disk,
            commitments,
        }
    }

    /// Returns the current network name as defined in BIP70 (main, test, regtest).
    pub fn chain(&self) -> String {
        self.chain.clone()
    }

    /// Returns the current number of blocks processed in the server.
    pub fn blocks(&self) -> Height {
        self.blocks
    }

    /// Returns the hash of the current best chain tip block, in big-endian order, hex-encoded.
    pub fn best_block_hash(&self) -> &block::Hash {
        &self.best_block_hash
    }

    /// Returns the estimated height of the chain.
    ///
    /// If syncing, the estimated height of the chain, else the current best height, numeric.
    ///
    /// In Zebra, this is always the height estimate, so it might be a little inaccurate.
    pub fn estimated_height(&self) -> Height {
        self.estimated_height
    }

    /// Returns the value pool balances.
    pub fn value_pools(&self) -> &[get_blockchain_info::Balance; 5] {
        &self.value_pools
    }

    /// Returns the network upgrades.
    pub fn upgrades(&self) -> &IndexMap<ConsensusBranchIdHex, NetworkUpgradeInfo> {
        &self.upgrades
    }

    /// Returns the Branch IDs of the current and upcoming consensus rules.
    pub fn consensus(&self) -> &TipConsensusBranch {
        &self.consensus
    }
}

/// A wrapper type with a list of transparent address strings.
///
/// This is used for the input parameter of [`RpcServer::get_address_balance`],
/// [`RpcServer::get_address_tx_ids`] and [`RpcServer::get_address_utxos`].
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(from = "DAddressStrings")]
pub struct AddressStrings {
    /// A list of transparent address strings.
    addresses: Vec<String>,
}

impl From<DAddressStrings> for AddressStrings {
    fn from(address_strings: DAddressStrings) -> Self {
        match address_strings {
            DAddressStrings::Addresses { addresses } => AddressStrings { addresses },
            DAddressStrings::Address(address) => AddressStrings {
                addresses: vec![address],
            },
        }
    }
}

/// An intermediate type used to deserialize [`AddressStrings`].
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Deserialize)]
#[serde(untagged)]
enum DAddressStrings {
    /// A list of address strings.
    Addresses { addresses: Vec<String> },
    /// A single address string.
    Address(String),
}

impl AddressStrings {
    /// Creates a new `AddressStrings` given a vector.
    #[cfg(test)]
    pub fn new(addresses: Vec<String>) -> AddressStrings {
        AddressStrings { addresses }
    }

    /// Creates a new [`AddressStrings`] from a given vector, returns an error if any addresses are incorrect.
    pub fn new_valid(addresses: Vec<String>) -> Result<AddressStrings> {
        let address_strings = Self { addresses };
        address_strings.clone().valid_addresses()?;
        Ok(address_strings)
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

    /// Given a list of addresses as strings:
    /// - check if provided list have all valid transparent addresses.
    /// - return valid addresses as a vec of strings.
    pub fn valid_address_strings(self) -> Result<Vec<String>> {
        self.clone().valid_addresses()?;
        Ok(self.addresses)
    }
}

/// The transparent balance of a set of addresses.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct AddressBalance {
    /// The total transparent balance.
    pub balance: u64,
    /// The total received balance, including change.
    pub received: u64,
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

impl SentTransactionHash {
    /// Constructs a new [`SentTransactionHash`].
    pub fn new(hash: transaction::Hash) -> Self {
        SentTransactionHash(hash)
    }

    /// Returns the contents of ['SentTransactionHash'].
    pub fn inner(&self) -> transaction::Hash {
        self.0
    }
}

/// Response to a `getblock` RPC request.
///
/// See the notes for the [`RpcServer::get_block`] method.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

        /// The blockcommitments field of the requested block. Its interpretation changes
        /// depending on the network and height.
        #[serde(with = "opthex", rename = "blockcommitments")]
        #[serde(skip_serializing_if = "Option::is_none")]
        block_commitments: Option<[u8; 32]>,

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
        /// List of transactions in block order, hex-encoded if verbosity=1 or
        /// as objects if verbosity=2.
        tx: Vec<GetBlockTransaction>,

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
        // `anchor` would be here. Not planned to be supported.
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
            block_commitments: None,
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
/// The transaction list in a `getblock` call. Can be a list of transaction
/// IDs or the full transaction details depending on verbosity.
pub enum GetBlockTransaction {
    /// The transaction hash, hex-encoded.
    Hash(#[serde(with = "hex")] transaction::Hash),
    /// The block object.
    Object(Box<TransactionObject>),
}

/// Response to a `getblockheader` RPC request.
///
/// See the notes for the [`RpcServer::get_block_header`] method.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum GetBlockHeader {
    /// The request block header, hex-encoded.
    Raw(hex_data::HexData),

    /// The block header object.
    Object(Box<GetBlockHeaderObject>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
/// Verbose response to a `getblockheader` RPC request.
///
/// See the notes for the [`RpcServer::get_block_header`] method.
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

    /// The blockcommitments field of the requested block. Its interpretation changes
    /// depending on the network and height.
    #[serde(with = "hex", rename = "blockcommitments")]
    pub block_commitments: [u8; 32],

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
    pub solution: Solution,

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
            block_commitments: Default::default(),
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
/// Also see the notes for the [`RpcServer::get_best_block_hash`] and `get_block_hash` methods.
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum GetRawTransaction {
    /// The raw transaction, encoded as hex bytes.
    Raw(#[serde(with = "hex")] SerializedTransaction),
    /// The transaction object.
    Object(Box<TransactionObject>),
}

impl Default for GetRawTransaction {
    fn default() -> Self {
        Self::Object(Box::default())
    }
}

/// Response to a `getaddressutxos` RPC request.
///
/// See the notes for the [`Rpc::get_address_utxos` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

impl GetAddressUtxos {
    /// Constructs a new instance of [`GetAddressUtxos`].
    pub fn from_parts(
        address: transparent::Address,
        txid: transaction::Hash,
        output_index: OutputIndex,
        script: transparent::Script,
        satoshis: u64,
        height: Height,
    ) -> Self {
        GetAddressUtxos {
            address,
            txid,
            output_index,
            script,
            satoshis,
            height,
        }
    }

    /// Returns the contents of [`GetAddressUtxos`].
    pub fn into_parts(
        &self,
    ) -> (
        transparent::Address,
        transaction::Hash,
        OutputIndex,
        transparent::Script,
        u64,
        Height,
    ) {
        (
            self.address.clone(),
            self.txid,
            self.output_index,
            self.script.clone(),
            self.satoshis,
            self.height,
        )
    }
}

/// A struct to use as parameter of the `getaddresstxids`.
///
/// See the notes for the [`Rpc::get_address_tx_ids` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GetAddressTxIdsRequest {
    // A list of addresses to get transactions from.
    addresses: Vec<String>,
    // The height to start looking for transactions.
    start: Option<u32>,
    // The height to end looking for transactions.
    end: Option<u32>,
}

impl GetAddressTxIdsRequest {
    /// Constructs [`GetAddressTxIdsRequest`] from its constituent parts.
    pub fn from_parts(addresses: Vec<String>, start: u32, end: u32) -> Self {
        GetAddressTxIdsRequest {
            addresses,
            start: Some(start),
            end: Some(end),
        }
    }
    /// Returns the contents of [`GetAddressTxIdsRequest`].
    pub fn into_parts(&self) -> (Vec<String>, u32, u32) {
        (
            self.addresses.clone(),
            self.start.unwrap_or(0),
            self.end.unwrap_or(0),
        )
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

/// Build a valid height range from the given optional start and end numbers.
///
/// # Parameters
///
/// - `start`: Optional starting height. If not provided, defaults to 0.
/// - `end`: Optional ending height. A value of 0 or absence of a value indicates to use `chain_height`.
/// - `chain_height`: The maximum permissible height.
///
/// # Returns
///
/// A `RangeInclusive<Height>` from the clamped start to the clamped end.
///
/// # Errors
///
/// Returns an error if the computed start is greater than the computed end.
fn build_height_range(
    start: Option<u32>,
    end: Option<u32>,
    chain_height: Height,
) -> Result<RangeInclusive<Height>> {
    // Convert optional values to Height, using 0 (as Height(0)) when missing.
    // If start is above chain_height, clamp it to chain_height.
    let start = Height(start.unwrap_or(0)).min(chain_height);

    // For `end`, treat a zero value or missing value as `chain_height`:
    let end = match end {
        Some(0) | None => chain_height,
        Some(val) => Height(val).min(chain_height),
    };

    if start > end {
        return Err(ErrorObject::owned(
            ErrorCode::InvalidParams.code(),
            format!("start {start:?} must be less than or equal to end {end:?}"),
            None::<()>,
        ));
    }

    Ok(start..=end)
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
            return Err(ErrorObject::borrowed(
                ErrorCode::InvalidParams.code(),
                "Provided index is greater than the current tip",
                None,
            ));
        }
        Ok(Height(height))
    } else {
        // `index + 1` can't overflow, because `index` is always negative here.
        let height = i32::try_from(tip_height.0)
            .expect("tip height fits in i32, because Height::MAX fits in i32")
            .checked_add(index + 1);

        let sanitized_height = match height {
            None => {
                return Err(ErrorObject::borrowed(
                    ErrorCode::InvalidParams.code(),
                    "Provided index is not valid",
                    None,
                ));
            }
            Some(h) => {
                if h < 0 {
                    return Err(ErrorObject::borrowed(
                        ErrorCode::InvalidParams.code(),
                        "Provided negative index ends up with a negative height",
                        None,
                    ));
                }
                let h: u32 = h.try_into().expect("Positive i32 always fits in u32");
                if h > tip_height.0 {
                    return Err(ErrorObject::borrowed(
                        ErrorCode::InvalidParams.code(),
                        "Provided index is greater than the current tip",
                        None,
                    ));
                }

                h
            }
        };

        Ok(Height(sanitized_height))
    }
}

/// A helper module to serialize and deserialize `Option<T: ToHex>` as a hex string.
pub mod opthex {
    use hex::{FromHex, ToHex};
    use serde::{de, Deserialize, Deserializer, Serializer};

    #[allow(missing_docs)]
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

    #[allow(missing_docs)]
    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: FromHex,
    {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt {
            Some(s) => T::from_hex(&s)
                .map(Some)
                .map_err(|_e| de::Error::custom("failed to convert hex string")),
            None => Ok(None),
        }
    }
}

/// A helper module to serialize and deserialize `[u8; N]` as a hex string.
pub mod arrayhex {
    use serde::{Deserializer, Serializer};
    use std::fmt;

    #[allow(missing_docs)]
    pub fn serialize<S, const N: usize>(data: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_string = hex::encode(data);
        serializer.serialize_str(&hex_string)
    }

    #[allow(missing_docs)]
    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<[u8; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HexArrayVisitor<const N: usize>;

        impl<const N: usize> serde::de::Visitor<'_> for HexArrayVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a hex string representing exactly {} bytes", N)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let vec = hex::decode(v).map_err(E::custom)?;
                vec.clone().try_into().map_err(|_| {
                    E::invalid_length(vec.len(), &format!("expected {} bytes", N).as_str())
                })
            }
        }

        deserializer.deserialize_str(HexArrayVisitor::<N>)
    }
}

/// Returns the proof-of-work difficulty as a multiple of the minimum difficulty.
pub async fn chain_tip_difficulty<State>(
    network: Network,
    mut state: State,
    should_use_default: bool,
) -> Result<f64>
where
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
{
    let request = ReadRequest::ChainInfo;

    // # TODO
    // - add a separate request like BestChainNextMedianTimePast, but skipping the
    //   consistency check, because any block's difficulty is ok for display
    // - return 1.0 for a "not enough blocks in the state" error, like `zcashd`:
    // <https://github.com/zcash/zcash/blob/7b28054e8b46eb46a9589d0bdc8e29f9fa1dc82d/src/rpc/blockchain.cpp#L40-L41>
    let response = state
        .ready()
        .and_then(|service| service.call(request))
        .await;

    let response = match (should_use_default, response) {
        (_, Ok(res)) => res,
        (true, Err(_)) => {
            return Ok((U256::from(network.target_difficulty_limit()) >> 128).as_u128() as f64)
        }
        (false, Err(error)) => return Err(ErrorObject::owned(0, error.to_string(), None::<()>)),
    };

    let chain_info = match response {
        ReadResponse::ChainInfo(info) => info,
        _ => unreachable!("unmatched response to a chain info request"),
    };

    // This RPC is typically used for display purposes, so it is not consensus-critical.
    // But it uses the difficulty consensus rules for its calculations.
    //
    // Consensus:
    // https://zips.z.cash/protocol/protocol.pdf#nbits
    //
    // The zcashd implementation performs to_expanded() on f64,
    // and then does an inverse division:
    // https://github.com/zcash/zcash/blob/d6e2fada844373a8554ee085418e68de4b593a6c/src/rpc/blockchain.cpp#L46-L73
    //
    // But in Zebra we divide the high 128 bits of each expanded difficulty. This gives
    // a similar result, because the lower 128 bits are insignificant after conversion
    // to `f64` with a 53-bit mantissa.
    //
    // `pow_limit >> 128 / difficulty >> 128` is the same as the work calculation
    // `(2^256 / pow_limit) / (2^256 / difficulty)`, but it's a bit more accurate.
    //
    // To simplify the calculation, we don't scale for leading zeroes. (Bitcoin's
    // difficulty currently uses 68 bits, so even it would still have full precision
    // using this calculation.)

    // Get expanded difficulties (256 bits), these are the inverse of the work
    let pow_limit: U256 = network.target_difficulty_limit().into();
    let Some(difficulty) = chain_info.expected_difficulty.to_expanded() else {
        return Ok(0.0);
    };

    // Shift out the lower 128 bits (256 bits, but the top 128 are all zeroes)
    let pow_limit = pow_limit >> 128;
    let difficulty = U256::from(difficulty) >> 128;

    // Convert to u128 then f64.
    // We could also convert U256 to String, then parse as f64, but that's slower.
    let pow_limit = pow_limit.as_u128() as f64;
    let difficulty = difficulty.as_u128() as f64;

    // Invert the division to give approximately: `work(difficulty) / work(pow_limit)`
    Ok(pow_limit / difficulty)
}

/// Commands for the `addnode` RPC method.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AddNodeCommand {
    /// Add a node to the address book.
    #[serde(rename = "add")]
    Add,
}
