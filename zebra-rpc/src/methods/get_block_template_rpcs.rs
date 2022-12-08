//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.

use std::{iter, sync::Arc};

use futures::{FutureExt, TryFutureExt};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::{
        self,
        merkle::{self, AuthDataRoot},
        Block, ChainHistoryBlockTxAuthCommitmentHash, Height, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION,
    },
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::{
    funding_stream_address, funding_stream_values, miner_subsidy, new_coinbase_script,
    VerifyChainError, MAX_BLOCK_SIGOPS,
};
use zebra_node_services::mempool;

use zebra_state::{ReadRequest, ReadResponse};

use crate::methods::{
    best_chain_tip_height,
    get_block_template_rpcs::types::{
        default_roots::DefaultRoots, get_block_template::GetBlockTemplate,
        get_block_template_opts::GetBlockTemplateRequestMode, hex_data::HexData, submit_block,
        transaction::TransactionTemplate,
    },
    GetBlockHash, MISSING_BLOCK_ERROR_CODE,
};

pub mod config;
pub mod constants;
pub mod types;
pub mod zip317;

/// The max estimated distance to the chain tip for the getblocktemplate method.
///
/// Allows the same clock skew as the Zcash network, which is 100 blocks, based on the standard rule:
/// > A full validator MUST NOT accept blocks with nTime more than two hours in the future
/// > according to its clock. This is not strictly a consensus rule because it is nondeterministic,
/// > and clock time varies between nodes.
const MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP: i32 = 100;

/// The default window size specifying how many blocks to check when estimating the chain's solution rate.
///
/// Based on default value in zcashd.
const DEFAULT_SOLUTION_RATE_WINDOW_SIZE: usize = 120;

/// The RPC error code used by `zcashd` for when it's still downloading initial blocks.
///
/// `s-nomp` mining pool expects error code `-10` when the node is not synced:
/// <https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L142>
pub const NOT_SYNCED_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-10);

/// getblocktemplate RPC method signatures.
#[rpc(server)]
pub trait GetBlockTemplateRpc {
    /// Returns the height of the most recent block in the best valid block chain (equivalently,
    /// the number of blocks in this chain excluding the genesis block).
    ///
    /// zcashd reference: [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)
    ///
    /// # Notes
    ///
    /// This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblockcount")]
    fn get_block_count(&self) -> Result<u32>;

    /// Returns the hash of the block of a given height iff the index argument correspond
    /// to a block in the best chain.
    ///
    /// zcashd reference: [`getblockhash`](https://zcash-rpc.github.io/getblockhash.html)
    ///
    /// # Parameters
    ///
    /// - `index`: (numeric, required) The block index.
    ///
    /// # Notes
    ///
    /// - If `index` is positive then index = block height.
    /// - If `index` is negative then -1 is the last known valid block.
    /// - This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblockhash")]
    fn get_block_hash(&self, index: i32) -> BoxFuture<Result<GetBlockHash>>;

    /// Returns a block template for mining new Zcash blocks.
    ///
    /// # Parameters
    ///
    /// - `jsonrequestobject`: (string, optional) A JSON object containing arguments.
    ///
    /// zcashd reference: [`getblocktemplate`](https://zcash-rpc.github.io/getblocktemplate.html)
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
    /// so moving between chains is very cheap. (But forking a new chain may take some time,
    /// until bug #4794 is fixed.)
    ///
    /// This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblocktemplate")]
    fn get_block_template(
        &self,
        options: Option<types::get_block_template_opts::JsonParameters>,
    ) -> BoxFuture<Result<GetBlockTemplate>>;

    /// Submits block to the node to be validated and committed.
    /// Returns the [`submit_block::Response`] for the operation, as a JSON string.
    ///
    /// zcashd reference: [`submitblock`](https://zcash.github.io/rpc/submitblock.html)
    ///
    /// # Parameters
    /// - `hexdata` (string, required)
    /// - `jsonparametersobject` (string, optional) - currently ignored
    ///  - holds a single field, workid, that must be included in submissions if provided by the server.
    #[rpc(name = "submitblock")]
    fn submit_block(
        &self,
        hex_data: HexData,
        _options: Option<submit_block::JsonParameters>,
    ) -> BoxFuture<Result<submit_block::Response>>;

    /// Returns mining-related information.
    ///
    /// zcashd reference: [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)
    #[rpc(name = "getmininginfo")]
    fn get_mining_info(&self) -> BoxFuture<Result<types::get_mining_info::Response>>;

    /// Returns the estimated network solutions per second based on the last `num_blocks` before `height`.
    /// If `num_blocks` is not supplied, uses 120 blocks.
    /// If `height` is not supplied or is 0, uses the tip height.
    ///
    /// zcashd reference: [`getnetworksolps`](https://zcash.github.io/rpc/getnetworksolps.html)
    #[rpc(name = "getnetworksolps")]
    fn get_network_sol_ps(
        &self,
        num_blocks: Option<usize>,
        height: Option<i32>,
    ) -> BoxFuture<Result<u64>>;

    /// Returns the estimated network solutions per second based on the last `num_blocks` before `height`.
    /// If `num_blocks` is not supplied, uses 120 blocks.
    /// If `height` is not supplied or is 0, uses the tip height.
    ///
    /// zcashd reference: [`getnetworkhashps`](https://zcash.github.io/rpc/getnetworkhashps.html)
    #[rpc(name = "getnetworkhashps")]
    fn get_network_hash_ps(
        &self,
        num_blocks: Option<usize>,
        height: Option<i32>,
    ) -> BoxFuture<Result<u64>> {
        self.get_network_sol_ps(num_blocks, height)
    }
}

/// RPC method implementations.
pub struct GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus>
where
    Mempool: Service<
        mempool::Request,
        Response = mempool::Response,
        Error = zebra_node_services::BoxError,
    >,
    State: Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
    ChainVerifier: Service<Arc<Block>, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // TODO: Add the other fields from the [`Rpc`] struct as-needed

    // Configuration
    //
    /// The configured network for this RPC service.
    network: Network,

    /// The configured miner address for this RPC service.
    ///
    /// Zebra currently only supports single-signature P2SH transparent addresses.
    miner_address: Option<transparent::Address>,

    // Services
    //
    /// A handle to the mempool service.
    mempool: Buffer<Mempool, mempool::Request>,

    /// A handle to the state service.
    state: State,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    /// The chain verifier, used for submitting blocks.
    chain_verifier: ChainVerifier,

    /// The chain sync status, used for checking if Zebra is likely close to the network chain tip.
    sync_status: SyncStatus,
}

impl<Mempool, State, Tip, ChainVerifier, SyncStatus>
    GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    ChainVerifier: Service<Arc<Block>, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the handler for getblocktemplate RPCs.
    pub fn new(
        network: Network,
        mining_config: config::Config,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        latest_chain_tip: Tip,
        chain_verifier: ChainVerifier,
        sync_status: SyncStatus,
    ) -> Self {
        Self {
            network,
            miner_address: mining_config.miner_address,
            mempool,
            state,
            latest_chain_tip,
            chain_verifier,
            sync_status,
        }
    }
}

impl<Mempool, State, Tip, ChainVerifier, SyncStatus> GetBlockTemplateRpc
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier, SyncStatus>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    ChainVerifier: Service<Arc<Block>, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ChainVerifier as Service<Arc<Block>>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    fn get_block_count(&self) -> Result<u32> {
        best_chain_tip_height(&self.latest_chain_tip).map(|height| height.0)
    }

    // TODO: use a generic error constructor (#5548)
    fn get_block_hash(&self, index: i32) -> BoxFuture<Result<GetBlockHash>> {
        let mut state = self.state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        async move {
            let tip_height = best_chain_tip_height(&latest_chain_tip)?;

            let height = get_height_from_int(index, tip_height)?;

            let request = zebra_state::ReadRequest::BestChainBlockHash(height);
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
                zebra_state::ReadResponse::BlockHash(Some(hash)) => Ok(GetBlockHash(hash)),
                zebra_state::ReadResponse::BlockHash(None) => Err(Error {
                    code: MISSING_BLOCK_ERROR_CODE,
                    message: "Block not found".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a block request"),
            }
        }
        .boxed()
    }

    // TODO: use HexData to handle block proposal data, and a generic error constructor (#5548)
    fn get_block_template(
        &self,
        options: Option<types::get_block_template_opts::JsonParameters>,
    ) -> BoxFuture<Result<GetBlockTemplate>> {
        let network = self.network;
        let miner_address = self.miner_address;

        let mempool = self.mempool.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let sync_status = self.sync_status.clone();
        let mut state = self.state.clone();

        // Since this is a very large RPC, we use separate functions for each group of fields.
        async move {
            if let Some(options) = options {
                if options.data.is_some() || options.mode == GetBlockTemplateRequestMode::Proposal {
                    return Err(Error {
                        code: ErrorCode::InvalidParams,
                        message: "\"proposal\" mode is currently unsupported by Zebra".to_string(),
                        data: None,
                    })
                }

                if options.longpollid.is_some() {
                    return Err(Error {
                        code: ErrorCode::InvalidParams,
                        message: "long polling is currently unsupported by Zebra".to_string(),
                        data: None,
                    })
                }
            }

            let miner_address = miner_address.ok_or_else(|| Error {
                code: ErrorCode::ServerError(0),
                message: "configure mining.miner_address in zebrad.toml \
                          with a transparent P2SH address"
                    .to_string(),
                data: None,
            })?;

            // The tip estimate may not be the same as the one coming from the state
            // but this is ok for an estimate
            let (estimated_distance_to_chain_tip, estimated_tip_height) = latest_chain_tip
                .estimate_distance_to_network_chain_tip(network)
                .ok_or_else(|| Error {
                    code: ErrorCode::ServerError(0),
                    message: "No Chain tip available yet".to_string(),
                    data: None,
                })?;

            if !sync_status.is_close_to_tip() || estimated_distance_to_chain_tip > MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP {
                tracing::info!(
                    estimated_distance_to_chain_tip,
                    ?estimated_tip_height,
                    "Zebra has not synced to the chain tip"
                );

                return Err(Error {
                    code: NOT_SYNCED_ERROR_CODE,
                    message: format!("Zebra has not synced to the chain tip, estimated distance: {estimated_distance_to_chain_tip}"),
                    data: None,
                });
            }

            // Calling state with `ChainInfo` request for relevant chain data
            let request = ReadRequest::ChainInfo;
            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                                data: None,
                            })?;

            let chain_info = match response {
                ReadResponse::ChainInfo(chain_info) => chain_info,
                _ => unreachable!("we should always have enough state data here to get a `GetBlockTemplateChainInfo`"),
            };

            // Get the tip data from the state call
            let block_height = (chain_info.tip_height + 1).expect("tip is far below Height::MAX");

            // Use a fake coinbase transaction to break the dependency between transaction
            // selection, the miner fee, and the fee payment in the coinbase transaction.
            let fake_coinbase_tx = fake_coinbase_transaction(network, block_height, miner_address);
            let mempool_txs = zip317::select_mempool_transactions(fake_coinbase_tx, mempool).await?;

            let miner_fee = miner_fee(&mempool_txs);

            let outputs =
                standard_coinbase_outputs(network, block_height, miner_address, miner_fee);
            let coinbase_tx = Transaction::new_v5_coinbase(network, block_height, outputs).into();

            let (merkle_root, auth_data_root) =
                calculate_transaction_roots(&coinbase_tx, &mempool_txs);

            let history_tree = chain_info.history_tree;
            // TODO: move expensive cryptography to a rayon thread?
            let chain_history_root = history_tree.hash().expect("history tree can't be empty");

            // TODO: move expensive cryptography to a rayon thread?
            let block_commitments_hash = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &chain_history_root,
                &auth_data_root,
            );

            // Convert into TransactionTemplates
            let mempool_txs = mempool_txs.iter().map(Into::into).collect();

            let mutable: Vec<String> = constants::GET_BLOCK_TEMPLATE_MUTABLE_FIELD.iter().map(ToString::to_string).collect();

            Ok(GetBlockTemplate {
                capabilities: Vec::new(),

                version: ZCASH_BLOCK_VERSION,

                previous_block_hash: GetBlockHash(chain_info.tip_hash),
                block_commitments_hash,
                light_client_root_hash: block_commitments_hash,
                final_sapling_root_hash: block_commitments_hash,
                default_roots: DefaultRoots {
                    merkle_root,
                    chain_history_root,
                    auth_data_root,
                    block_commitments_hash,
                },

                transactions: mempool_txs,

                coinbase_txn: TransactionTemplate::from_coinbase(&coinbase_tx, miner_fee),

                target: format!(
                    "{}",
                    chain_info.expected_difficulty
                        .to_expanded()
                        .expect("state always returns a valid difficulty value")
                ),

                min_time: chain_info.min_time.timestamp(),

                mutable,

                nonce_range: constants::GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD.to_string(),

                sigop_limit: MAX_BLOCK_SIGOPS,

                size_limit: MAX_BLOCK_BYTES,

                cur_time: chain_info.cur_time.timestamp(),

                bits: format!("{:#010x}", chain_info.expected_difficulty.to_value())
                    .drain(2..)
                    .collect(),

                height: block_height.0,

                max_time: chain_info.max_time.timestamp(),
            })
        }
        .boxed()
    }

    fn submit_block(
        &self,
        HexData(block_bytes): HexData,
        _options: Option<submit_block::JsonParameters>,
    ) -> BoxFuture<Result<submit_block::Response>> {
        let mut chain_verifier = self.chain_verifier.clone();

        async move {
            let block: Block = match block_bytes.zcash_deserialize_into() {
                Ok(block_bytes) => block_bytes,
                Err(_) => return Ok(submit_block::ErrorResponse::Rejected.into()),
            };

            let chain_verifier_response = chain_verifier
                .ready()
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?
                .call(Arc::new(block))
                .await;

            let chain_error = match chain_verifier_response {
                // Currently, this match arm returns `null` (Accepted) for blocks committed
                // to any chain, but Accepted is only for blocks in the best chain.
                //
                // TODO (#5487):
                // - Inconclusive: check if the block is on a side-chain
                // The difference is important to miners, because they want to mine on the best chain.
                Ok(_block_hash) => return Ok(submit_block::Response::Accepted),

                // Turns BoxError into Result<VerifyChainError, BoxError>,
                // by downcasting from Any to VerifyChainError.
                Err(box_error) => box_error
                    .downcast::<VerifyChainError>()
                    .map(|boxed_chain_error| *boxed_chain_error),
            };

            let response = match chain_error {
                Ok(source) if source.is_duplicate_request() => {
                    submit_block::ErrorResponse::Duplicate
                }

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
                // Checking the download queues and ChainVerifier buffer for duplicates
                // might require architectural changes to Zebra, so we should only do it
                // if mining pools really need it.
                Ok(_verify_chain_error) => submit_block::ErrorResponse::Rejected,

                // This match arm is currently unreachable, but if future changes add extra error types,
                // we want to turn them into `Rejected`.
                Err(_unknown_error_type) => submit_block::ErrorResponse::Rejected,
            };

            Ok(response.into())
        }
        .boxed()
    }

    fn get_mining_info(&self) -> BoxFuture<Result<types::get_mining_info::Response>> {
        let network = self.network;
        let solution_rate_fut = self.get_network_sol_ps(None, None);
        async move {
            Ok(types::get_mining_info::Response::new(
                network,
                solution_rate_fut.await?,
            ))
        }
        .boxed()
    }

    fn get_network_sol_ps(
        &self,
        num_blocks: Option<usize>,
        height: Option<i32>,
    ) -> BoxFuture<Result<u64>> {
        let num_blocks = num_blocks
            .map(|num_blocks| num_blocks.max(1))
            .unwrap_or(DEFAULT_SOLUTION_RATE_WINDOW_SIZE);
        let height = height.and_then(|height| (height > 1).then_some(Height(height as u32)));
        let mut state = self.state.clone();

        async move {
            let request = ReadRequest::SolutionRate { num_blocks, height };

            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            let solution_rate = match response {
                ReadResponse::SolutionRate(solution_rate) => solution_rate.ok_or(Error {
                    code: ErrorCode::ServerError(0),
                    message: "No blocks in state".to_string(),
                    data: None,
                })?,
                _ => unreachable!("unmatched response to a solution rate request"),
            };

            Ok(solution_rate
                .try_into()
                .expect("per-second solution rate always fits in u64"))
        }
        .boxed()
    }
}

// get_block_template support methods

/// Returns the total miner fee for `mempool_txs`.
pub fn miner_fee(mempool_txs: &[VerifiedUnminedTx]) -> Amount<NonNegative> {
    let miner_fee: amount::Result<Amount<NonNegative>> =
        mempool_txs.iter().map(|tx| tx.miner_fee).sum();

    miner_fee.expect(
        "invalid selected transactions: \
         fees in a valid block can not be more than MAX_MONEY",
    )
}

/// Returns the standard funding stream and miner reward transparent output scripts
/// for `network`, `height` and `miner_fee`.
///
/// Only works for post-Canopy heights.
pub fn standard_coinbase_outputs(
    network: Network,
    height: Height,
    miner_address: transparent::Address,
    miner_fee: Amount<NonNegative>,
) -> Vec<(Amount<NonNegative>, transparent::Script)> {
    let funding_streams = funding_stream_values(height, network)
        .expect("funding stream value calculations are valid for reasonable chain heights");

    let mut funding_streams: Vec<(Amount<NonNegative>, transparent::Address)> = funding_streams
        .iter()
        .map(|(receiver, amount)| (*amount, funding_stream_address(height, network, *receiver)))
        .collect();
    // The HashMap returns funding streams in an arbitrary order,
    // but Zebra's snapshot tests expect the same order every time.
    funding_streams.sort_by_key(|(amount, _address)| *amount);

    let miner_reward = miner_subsidy(height, network)
        .expect("reward calculations are valid for reasonable chain heights")
        + miner_fee;
    let miner_reward =
        miner_reward.expect("reward calculations are valid for reasonable chain heights");

    let mut coinbase_outputs = funding_streams;
    coinbase_outputs.push((miner_reward, miner_address));

    coinbase_outputs
        .iter()
        .map(|(amount, address)| (*amount, new_coinbase_script(*address)))
        .collect()
}

/// Returns a fake coinbase transaction that can be used during transaction selection.
///
/// This avoids a data dependency loop involving the selected transactions, the miner fee,
/// and the coinbase transaction.
///
/// This transaction's serialized size and sigops must be at least as large as the real coinbase
/// transaction with the correct height and fee.
fn fake_coinbase_transaction(
    network: Network,
    block_height: Height,
    miner_address: transparent::Address,
) -> TransactionTemplate<NegativeOrZero> {
    // Block heights are encoded as variable-length (script) and `u32` (lock time, expiry height).
    // They can also change the `u32` consensus branch id.
    // We use the template height here, which has the correct byte length.
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    // https://github.com/zcash/zips/blob/main/zip-0203.rst#changes-for-nu5
    //
    // Transparent amounts are encoded as `i64`,
    // so one zat has the same size as the real amount:
    // https://developer.bitcoin.org/reference/transactions.html#txout-a-transaction-output
    let miner_fee = 1.try_into().expect("amount is valid and non-negative");

    let outputs = standard_coinbase_outputs(network, block_height, miner_address, miner_fee);
    let coinbase_tx = Transaction::new_v5_coinbase(network, block_height, outputs).into();

    TransactionTemplate::from_coinbase(&coinbase_tx, miner_fee)
}

/// Returns the transaction effecting and authorizing roots
/// for `coinbase_tx` and `mempool_txs`.
//
// TODO: should this be spawned into a cryptographic operations pool?
//       (it would only matter if there were a lot of small transactions in a block)
pub fn calculate_transaction_roots(
    coinbase_tx: &UnminedTx,
    mempool_txs: &[VerifiedUnminedTx],
) -> (merkle::Root, AuthDataRoot) {
    let block_transactions =
        || iter::once(coinbase_tx).chain(mempool_txs.iter().map(|tx| &tx.transaction));

    let merkle_root = block_transactions().cloned().collect();
    let auth_data_root = block_transactions().cloned().collect();

    (merkle_root, auth_data_root)
}

// get_block_hash support methods

/// Given a potentially negative index, find the corresponding `Height`.
///
/// This function is used to parse the integer index argument of `get_block_hash`.
fn get_height_from_int(index: i32, tip_height: Height) -> Result<Height> {
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
