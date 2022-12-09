//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.

use std::sync::Arc;

use futures::{FutureExt, TryFutureExt};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    block::{
        self, Block, ChainHistoryBlockTxAuthCommitmentHash, Height, MAX_BLOCK_BYTES,
        ZCASH_BLOCK_VERSION,
    },
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transparent,
};
use zebra_consensus::{VerifyChainError, MAX_BLOCK_SIGOPS};
use zebra_node_services::mempool;
use zebra_state::{ReadRequest, ReadResponse};

use crate::methods::{
    best_chain_tip_height,
    get_block_template_rpcs::{
        constants::{
            DEFAULT_SOLUTION_RATE_WINDOW_SIZE, GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD,
            GET_BLOCK_TEMPLATE_MUTABLE_FIELD, GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD,
        },
        get_block_template::{
            calculate_miner_fee, calculate_transaction_roots, check_address,
            check_block_template_parameters, check_synced_to_tip, fetch_mempool_transactions,
            fetch_state_tip_and_local_time, generate_coinbase_transaction,
        },
        types::{
            default_roots::DefaultRoots, get_block_template::GetBlockTemplate, get_mining_info,
            hex_data::HexData, long_poll::LongPollInput, submit_block,
            transaction::TransactionTemplate,
        },
    },
    height_from_signed_int, GetBlockHash, MISSING_BLOCK_ERROR_CODE,
};

pub mod config;
pub mod constants;
pub mod get_block_template;
pub mod types;
pub mod zip317;

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
        parameters: Option<get_block_template::JsonParameters>,
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
        _parameters: Option<submit_block::JsonParameters>,
    ) -> BoxFuture<Result<submit_block::Response>>;

    /// Returns mining-related information.
    ///
    /// zcashd reference: [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)
    #[rpc(name = "getmininginfo")]
    fn get_mining_info(&self) -> BoxFuture<Result<get_mining_info::Response>>;

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
    // Configuration
    //
    /// The configured network for this RPC service.
    network: Network,

    /// The configured miner address for this RPC service.
    ///
    /// Zebra currently only supports transparent addresses.
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
            // TODO: look up this height as part of the state request?
            let tip_height = best_chain_tip_height(&latest_chain_tip)?;

            let height = height_from_signed_int(index, tip_height)?;

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
        parameters: Option<get_block_template::JsonParameters>,
    ) -> BoxFuture<Result<GetBlockTemplate>> {
        // Clone Config
        let network = self.network;
        let miner_address = self.miner_address;

        // Clone Services
        let mempool = self.mempool.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let sync_status = self.sync_status.clone();
        let state = self.state.clone();

        // To implement long polling correctly, we split this RPC into multiple phases.
        async move {
            // - Once-off checks

            // Check config and parameters.
            // These checks always have the same result during long polling.
            let miner_address = check_address(miner_address)?;

            if let Some(parameters) = parameters {
                check_block_template_parameters(parameters)?;
            }

            // - Checks and fetches that can change during long polling

            // Check if we are synced to the tip.
            // The result of this check can change during long polling.
            check_synced_to_tip(network, latest_chain_tip, sync_status)?;

            // Fetch the state data and local time for the block template:
            // - if the tip block hash changes, we must return from long polling,
            // - if the local clock changes on testnet, we might return from long polling
            //
            // We also return after 90 minutes on mainnet, even if we have the same response.
            let chain_tip_and_local_time = fetch_state_tip_and_local_time(state).await?;

            // Fetch the mempool data for the block template:
            // - if the mempool transactions change, we might return from long polling.
            let mempool_txs = fetch_mempool_transactions(mempool).await?;

            // - Long poll ID calculation

            let long_poll_id = LongPollInput::new(
                chain_tip_and_local_time.tip_height,
                chain_tip_and_local_time.tip_hash,
                chain_tip_and_local_time.max_time,
                mempool_txs.iter().map(|tx| tx.transaction.id),
            )
            .into();

            // - Processing fetched data to create a transaction template
            //
            // Apart from random weighted transaction selection,
            // the template only depends on the previously fetched data.

            // Calculate the next block height.
            let next_block_height =
                (chain_tip_and_local_time.tip_height + 1).expect("tip is far below Height::MAX");

            // Randomly select some mempool transactions.
            //
            // TODO: sort these transactions to match zcashd's order, to make testing easier.
            let mempool_txs = zip317::select_mempool_transactions(
                network,
                next_block_height,
                miner_address,
                mempool_txs,
            )
            .await;

            // Generate the coinbase transaction
            let miner_fee = calculate_miner_fee(&mempool_txs);
            let coinbase_tx =
                generate_coinbase_transaction(network, next_block_height, miner_address, miner_fee);

            // TODO: move into a new block_roots()

            // TODO: move expensive root, hash, and tree cryptography to a rayon thread?
            let (merkle_root, auth_data_root) =
                calculate_transaction_roots(&coinbase_tx, &mempool_txs);

            let history_tree = chain_tip_and_local_time.history_tree;
            let chain_history_root = history_tree.hash().expect("history tree can't be empty");

            let block_commitments_hash = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &chain_history_root,
                &auth_data_root,
            );

            // Convert into TransactionTemplates
            let mempool_txs = mempool_txs.iter().map(Into::into).collect();

            // TODO: make these serialize without this complex code?

            let capabilities: Vec<String> = GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD
                .iter()
                .map(ToString::to_string)
                .collect();
            let mutable: Vec<String> = GET_BLOCK_TEMPLATE_MUTABLE_FIELD
                .iter()
                .map(ToString::to_string)
                .collect();

            Ok(GetBlockTemplate {
                capabilities,

                version: ZCASH_BLOCK_VERSION,

                previous_block_hash: GetBlockHash(chain_tip_and_local_time.tip_hash),
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

                long_poll_id,

                // TODO: move this into another function or make it happen on serialization
                target: format!(
                    "{}",
                    chain_tip_and_local_time
                        .expected_difficulty
                        .to_expanded()
                        .expect("state always returns a valid difficulty value")
                ),

                min_time: chain_tip_and_local_time.min_time.timestamp(),

                mutable,

                // TODO: make this conversion happen automatically?
                nonce_range: GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD.to_string(),

                sigop_limit: MAX_BLOCK_SIGOPS,

                size_limit: MAX_BLOCK_BYTES,

                cur_time: chain_tip_and_local_time.cur_time.timestamp(),

                // TODO: move this into another function or make it happen on serialization
                bits: format!(
                    "{:#010x}",
                    chain_tip_and_local_time.expected_difficulty.to_value()
                )
                .drain(2..)
                .collect(),

                height: next_block_height.0,

                max_time: chain_tip_and_local_time.max_time.timestamp(),
            })
        }
        .boxed()
    }

    fn submit_block(
        &self,
        HexData(block_bytes): HexData,
        _parameters: Option<submit_block::JsonParameters>,
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

    fn get_mining_info(&self) -> BoxFuture<Result<get_mining_info::Response>> {
        let network = self.network;
        let solution_rate_fut = self.get_network_sol_ps(None, None);
        async move {
            Ok(get_mining_info::Response::new(
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

// Put support functions in a submodule, to keep this file small.
