//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.

use std::{iter, sync::Arc};

use chrono::Duration;
use futures::{FutureExt, TryFutureExt};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::Height,
    block::{
        self,
        merkle::{self, AuthDataRoot},
        Block, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION,
    },
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::{
    funding_stream_address, funding_stream_values, miner_subsidy, new_coinbase_script, BlockError,
    VerifyBlockError, VerifyChainError, VerifyCheckpointError, MAX_BLOCK_SIGOPS,
};
use zebra_node_services::mempool;

use zebra_state::{ReadRequest, ReadResponse};

use crate::methods::{
    best_chain_tip_height,
    get_block_template_rpcs::types::{
        default_roots::DefaultRoots, get_block_template::GetBlockTemplate, hex_data::HexData,
        submit_block, transaction::TransactionTemplate,
    },
    GetBlockHash, MISSING_BLOCK_ERROR_CODE,
};

pub mod config;
pub mod constants;
pub(crate) mod types;

/// The max estimated distance to the chain tip for the getblocktemplate method
// Set to 30 in case the local time is a little ahead.
// TODO: Replace this with SyncStatus
const MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP: i32 = 30;

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
    fn get_block_template(&self) -> BoxFuture<Result<GetBlockTemplate>>;

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
}

/// RPC method implementations.
pub struct GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier>
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
}

impl<Mempool, State, Tip, ChainVerifier> GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier>
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
{
    /// Create a new instance of the handler for getblocktemplate RPCs.
    pub fn new(
        network: Network,
        mining_config: config::Config,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        latest_chain_tip: Tip,
        chain_verifier: ChainVerifier,
    ) -> Self {
        Self {
            network,
            miner_address: mining_config.miner_address,
            mempool,
            state,
            latest_chain_tip,
            chain_verifier,
        }
    }
}

impl<Mempool, State, Tip, ChainVerifier> GetBlockTemplateRpc
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, ChainVerifier>
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
{
    fn get_block_count(&self) -> Result<u32> {
        best_chain_tip_height(&self.latest_chain_tip).map(|height| height.0)
    }

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

    fn get_block_template(&self) -> BoxFuture<Result<GetBlockTemplate>> {
        let network = self.network;
        let miner_address = self.miner_address;

        let mempool = self.mempool.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let mut state = self.state.clone();

        // Since this is a very large RPC, we use separate functions for each group of fields.
        async move {
            let miner_address = miner_address.ok_or_else(|| Error {
                code: ErrorCode::ServerError(0),
                message: "configure mining.miner_address in zebrad.toml \
                          with a transparent P2SH single signature address"
                    .to_string(),
                data: None,
            })?;

            // The tip estimate must not be the same as the one coming from the state
            // but this is ok for an estimate
            let (estimated_distance_to_chain_tip, tip_height) = latest_chain_tip
                .estimate_distance_to_network_chain_tip(network)
                .ok_or_else(|| Error {
                    code: ErrorCode::ServerError(0),
                    message: "No Chain tip available yet".to_string(),
                    data: None,
                })?;

            if estimated_distance_to_chain_tip > MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP {
                tracing::info!(
                    estimated_distance_to_chain_tip,
                    ?tip_height,
                    "Zebra has not synced to the chain tip"
                );

                return Err(Error {
                    // Return error code -10 (https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L140)
                    // TODO: Confirm that this is the expected error code for !synced
                    code: ErrorCode::ServerError(-10),
                    message: format!("Zebra has not synced to the chain tip, estimated distance: {estimated_distance_to_chain_tip}"),
                    data: None,
                });
            }

            let mempool_txs = select_mempool_transactions(mempool).await?;

            let miner_fee = miner_fee(&mempool_txs);

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
                ReadResponse::ChainInfo(Some(chain_info)) => chain_info,
                _ => unreachable!("lets hope for always some until later"),
            };

            // Get the tip data from the state call
            let tip_height = chain_info.tip.0;
            let tip_hash = chain_info.tip.1;

            let block_height = (tip_height + 1).expect("tip is far below Height::MAX");
            let min_time = chain_info.median_time_past.checked_add_signed(Duration::seconds(1))
                .expect("median time plus a small constant is far below i64::MAX");

            // > For each block at block height 2 or greater on Mainnet, or block height 653606 or greater on Testnet, nTime
            // > MUST be less than or equal to the median-time-past of that block plus 90 * 60 seconds.
            //
            // We ignore the height as we are checkpointing on nu5 in Mainnet and Testnet.
            let cur_time = chain_info.current_system_time;
            if cur_time > min_time.checked_add_signed(Duration::seconds(5400)).expect("median time plus a small constant is far below i64::MAX") {
                return Err(Error {
                    code: ErrorCode::ServerError(0),
                    message: format!("Current time {cur_time} is bigger than {min_time} + 5400 seconds."),
                    data: None,
                });
            }

            let outputs =
                standard_coinbase_outputs(network, block_height, miner_address, miner_fee);
            let coinbase_tx = Transaction::new_v5_coinbase(network, block_height, outputs).into();

            let (merkle_root, auth_data_root) =
                calculate_transaction_roots(&coinbase_tx, &mempool_txs);

            // Convert into TransactionTemplates
            let mempool_txs = mempool_txs.iter().map(Into::into).collect();

            Ok(GetBlockTemplate {
                capabilities: Vec::new(),

                version: ZCASH_BLOCK_VERSION,

                previous_block_hash: GetBlockHash(tip_hash),
                block_commitments_hash: [0; 32].into(),
                light_client_root_hash: [0; 32].into(),
                final_sapling_root_hash: [0; 32].into(),
                default_roots: DefaultRoots {
                    merkle_root,
                    chain_history_root: [0; 32].into(),
                    auth_data_root,
                    block_commitments_hash: [0; 32].into(),
                },

                transactions: mempool_txs,

                coinbase_txn: TransactionTemplate::from_coinbase(&coinbase_tx, miner_fee),

                target: format!(
                    "{}",
                    chain_info.expected_difficulty
                        .to_expanded()
                        .ok_or(Error {
                            code: ErrorCode::ServerError(0),
                            message: "Not enough blocks in the chain".to_string(),
                            data: None,
                        })?
                ),

                min_time: min_time.timestamp(),

                mutable: constants::GET_BLOCK_TEMPLATE_MUTABLE_FIELD
                    .iter()
                    .map(ToString::to_string)
                    .collect(),

                nonce_range: constants::GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD.to_string(),

                sigop_limit: MAX_BLOCK_SIGOPS,

                size_limit: MAX_BLOCK_BYTES,

                cur_time: cur_time.timestamp(),

                bits: format!("{:#010x}", chain_info.expected_difficulty.to_value())
                    .drain(2..)
                    .collect(),

                height: block_height.0,
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

            Ok(match chain_error {
                Ok(
                    VerifyChainError::Checkpoint(VerifyCheckpointError::AlreadyVerified { .. })
                    | VerifyChainError::Block(VerifyBlockError::Block {
                        source: BlockError::AlreadyInChain(..),
                    }),
                ) => submit_block::ErrorResponse::Duplicate,

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
            }
            .into())
        }
        .boxed()
    }
}

// get_block_template support methods

/// Returns selected transactions in the `mempool`, or an error if the mempool has failed.
///
/// TODO: select transactions according to ZIP-317 (#5473)
pub async fn select_mempool_transactions<Mempool>(
    mempool: Mempool,
) -> Result<Vec<VerifiedUnminedTx>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
{
    let response = mempool
        .oneshot(mempool::Request::FullTransactions)
        .await
        .map_err(|error| Error {
            code: ErrorCode::ServerError(0),
            message: error.to_string(),
            data: None,
        })?;

    if let mempool::Response::FullTransactions(transactions) = response {
        // TODO: select transactions according to ZIP-317 (#5473)
        Ok(transactions)
    } else {
        unreachable!("unmatched response to a mempool::FullTransactions request");
    }
}

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
