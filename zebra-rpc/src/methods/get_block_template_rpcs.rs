//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.

use std::{sync::Arc, time::Duration};

use futures::{future::OptionFuture, FutureExt, TryFutureExt};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transparent,
};
use zebra_consensus::VerifyChainError;
use zebra_node_services::mempool;
use zebra_state::{ReadRequest, ReadResponse};

use crate::methods::{
    best_chain_tip_height,
    get_block_template_rpcs::{
        constants::{
            DEFAULT_SOLUTION_RATE_WINDOW_SIZE, GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
        },
        get_block_template::{
            check_block_template_parameters, check_miner_address, check_synced_to_tip,
            fetch_mempool_transactions, fetch_state_tip_and_local_time,
            generate_coinbase_and_roots,
        },
        types::{
            get_block_template::GetBlockTemplate, get_mining_info, hex_data::HexData,
            long_poll::LongPollInput, submit_block,
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
        let mut latest_chain_tip = self.latest_chain_tip.clone();
        let sync_status = self.sync_status.clone();
        let state = self.state.clone();

        // To implement long polling correctly, we split this RPC into multiple phases.
        async move {
            // - One-off checks

            // Check config and parameters.
            // These checks always have the same result during long polling.
            let miner_address = check_miner_address(miner_address)?;

            let mut client_long_poll_id = None;
            if let Some(parameters) = parameters {
                check_block_template_parameters(&parameters)?;

                client_long_poll_id = parameters.long_poll_id;
            }

            // - Checks and fetches that can change during long polling
            //
            // Set up the loop.
            let mut max_time_reached = false;

            // The loop returns the server long poll ID,
            // which should be different to the client long poll ID.
            let (server_long_poll_id, chain_tip_and_local_time, mempool_txs, submit_old) = loop {
                // Check if we are synced to the tip.
                // The result of this check can change during long polling.
                //
                // Optional TODO:
                // - add `async changed()` method to ChainSyncStatus (like `ChainTip`)
                check_synced_to_tip(network, latest_chain_tip.clone(), sync_status.clone())?;

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
                let chain_tip_and_local_time =
                    fetch_state_tip_and_local_time(state.clone()).await?;

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
                let mempool_txs = fetch_mempool_transactions(mempool.clone()).await?;

                // - Long poll ID calculation
                let server_long_poll_id = LongPollInput::new(
                    chain_tip_and_local_time.tip_height,
                    chain_tip_and_local_time.tip_hash,
                    chain_tip_and_local_time.max_time,
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
                let wait_for_mempool_request = tokio::time::sleep(Duration::from_secs(
                    GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
                ));

                // Return immediately if the chain tip has changed.
                let wait_for_best_tip_change = latest_chain_tip.best_tip_changed();

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
                let duration_until_max_time = chain_tip_and_local_time
                    .max_time
                    .saturating_duration_since(chain_tip_and_local_time.cur_time);
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

                // TODO: change logging to debug after testing
                tokio::select! {
                    // Poll the futures in the listed order, for efficiency.
                    // We put the most frequent conditions first.
                    biased;

                    // This timer elapses every few seconds
                    _elapsed = wait_for_mempool_request => {
                        tracing::info!(
                            max_time = ?chain_tip_and_local_time.max_time,
                            cur_time = ?chain_tip_and_local_time.cur_time,
                            ?server_long_poll_id,
                            ?client_long_poll_id,
                            GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
                            "checking for a new mempool change after waiting a few seconds"
                        );
                    }

                    // The state changes after around a target block interval (75s)
                    tip_changed_result = wait_for_best_tip_change => {
                        match tip_changed_result {
                            Ok(()) => {
                                tracing::info!(
                                    max_time = ?chain_tip_and_local_time.max_time,
                                    cur_time = ?chain_tip_and_local_time.cur_time,
                                    ?server_long_poll_id,
                                    ?client_long_poll_id,
                                    "returning from long poll because state has changed"
                                );
                            }

                            Err(recv_error) => {
                                // This log should stay at info when the others go to debug,
                                // it will help with debugging.
                                tracing::info!(
                                    ?recv_error,
                                    max_time = ?chain_tip_and_local_time.max_time,
                                    cur_time = ?chain_tip_and_local_time.cur_time,
                                    ?server_long_poll_id,
                                    ?client_long_poll_id,
                                    "returning from long poll due to a state error.\
                                     Is Zebra shutting down?"
                                );

                                return Err(Error {
                                    code: ErrorCode::ServerError(0),
                                    message: recv_error.to_string(),
                                    data: None,
                                });
                            }
                        }
                    }

                    // The max time does not elapse during normal operation on mainnet,
                    // and it rarely elapses on testnet.
                    Some(_elapsed) = wait_for_max_time => {
                        // This log should stay at info when the others go to debug,
                        // it's very rare.
                        tracing::info!(
                            max_time = ?chain_tip_and_local_time.max_time,
                            cur_time = ?chain_tip_and_local_time.cur_time,
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

            // - After this point, the template only depends on the previously fetched data.

            // Generate the coinbase transaction and default roots
            //
            // TODO: move expensive root, hash, and tree cryptography to a rayon thread?
            let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
                network,
                next_block_height,
                miner_address,
                &mempool_txs,
                chain_tip_and_local_time.history_tree.clone(),
            );

            let response = GetBlockTemplate::new(
                next_block_height,
                &chain_tip_and_local_time,
                server_long_poll_id,
                coinbase_txn,
                &mempool_txs,
                default_roots,
                submit_old,
            );

            Ok(response)
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
