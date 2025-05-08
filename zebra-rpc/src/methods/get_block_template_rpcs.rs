//! Mining-related RPCs.

use std::{fmt::Debug, sync::Arc, time::Duration};

use futures::{future::OptionFuture, TryFutureExt};
use jsonrpsee::core::{async_trait, RpcResult as Result};
use jsonrpsee_proc_macros::rpc;
use jsonrpsee_types::ErrorObject;
use tokio::sync::watch;
use tower::{Service, ServiceExt};

use zcash_address::{unified::Encoding, TryFromAddress};

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::{self, Block, Height, TryIntoHeight},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::{
        subsidy::{
            block_subsidy, funding_stream_values, miner_subsidy, FundingStreamReceiver,
            ParameterSubsidy,
        },
        Network, NetworkKind, NetworkUpgrade, POW_AVERAGING_WINDOW,
    },
    primitives,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::{
        self, EXTRA_ZEBRA_COINBASE_DATA, MAX_COINBASE_DATA_LEN, MAX_COINBASE_HEIGHT_DATA_LEN,
    },
};
use zebra_consensus::{funding_stream_address, RouterError};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;
use zebra_state::{ReadRequest, ReadResponse};

use crate::{
    methods::{
        best_chain_tip_height, chain_tip_difficulty,
        get_block_template_rpcs::{
            constants::{
                DEFAULT_SOLUTION_RATE_WINDOW_SIZE, GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
                ZCASHD_FUNDING_STREAM_ORDER,
            },
            get_block_template::{
                check_miner_address, check_synced_to_tip, fetch_mempool_transactions,
                fetch_state_tip_and_local_time, validate_block_proposal,
            },
            // TODO: move the types/* modules directly under get_block_template_rpcs,
            //       and combine any modules with the same names.
            types::{
                get_block_template::{
                    proposal::TimeSource, proposal_block_from_template, GetBlockTemplate,
                },
                get_mining_info,
                long_poll::LongPollInput,
                peer_info::PeerInfo,
                submit_block,
                subsidy::{BlockSubsidy, FundingStream},
                unified_address, validate_address, z_validate_address,
            },
        },
        height_from_signed_int,
        hex_data::HexData,
        GetBlockHash,
    },
    server::{
        self,
        error::{MapError, OkOrError},
    },
};

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
        parameters: Option<get_block_template::JsonParameters>,
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

    /// Checks if a zcash address is valid.
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

    /// Checks if a zcash address is valid.
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
    async fn z_validate_address(
        &self,
        address: String,
    ) -> Result<types::z_validate_address::Response>;

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
}

/// RPC method implementations.
#[derive(Clone)]
pub struct GetBlockTemplateRpcImpl<
    Mempool,
    State,
    Tip,
    BlockVerifierRouter,
    SyncStatus,
    AddressBook,
> where
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
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    // Configuration
    //
    /// The configured network for this RPC service.
    network: Network,

    /// The configured miner address for this RPC service.
    ///
    /// Zebra currently only supports transparent addresses.
    miner_address: Option<transparent::Address>,

    /// Extra data to include in coinbase transaction inputs.
    /// Limited to around 95 bytes by the consensus rules.
    extra_coinbase_data: Vec<u8>,

    /// Should Zebra's block templates try to imitate `zcashd`?
    /// Developer-only config.
    debug_like_zcashd: bool,

    // Services
    //
    /// A handle to the mempool service.
    mempool: Mempool,

    /// A handle to the state service.
    state: State,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    /// The chain verifier, used for submitting blocks.
    block_verifier_router: BlockVerifierRouter,

    /// The chain sync status, used for checking if Zebra is likely close to the network chain tip.
    sync_status: SyncStatus,

    /// Address book of peers, used for `getpeerinfo`.
    address_book: AddressBook,

    /// A channel to send successful block submissions to the block gossip task,
    /// so they can be advertised to peers.
    mined_block_sender: watch::Sender<(block::Hash, block::Height)>,
}

impl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook> Debug
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>
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
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Skip fields without debug impls
        f.debug_struct("GetBlockTemplateRpcImpl")
            .field("network", &self.network)
            .field("miner_address", &self.miner_address)
            .field("extra_coinbase_data", &self.extra_coinbase_data)
            .field("debug_like_zcashd", &self.debug_like_zcashd)
            .finish()
    }
}

impl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>
    GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>
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
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the handler for getblocktemplate RPCs.
    ///
    /// # Panics
    ///
    /// If the `mining_config` is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network: &Network,
        mining_config: crate::config::mining::Config,
        mempool: Mempool,
        state: State,
        latest_chain_tip: Tip,
        block_verifier_router: BlockVerifierRouter,
        sync_status: SyncStatus,
        address_book: AddressBook,
        mined_block_sender: Option<watch::Sender<(block::Hash, block::Height)>>,
    ) -> Self {
        // Prevent loss of miner funds due to an unsupported or incorrect address type.
        if let Some(miner_address) = mining_config.miner_address.clone() {
            match network.kind() {
                NetworkKind::Mainnet => assert_eq!(
                    miner_address.network_kind(),
                    NetworkKind::Mainnet,
                    "Incorrect config: Zebra is configured to run on a Mainnet network, \
                    which implies the configured mining address needs to be for Mainnet, \
                    but the provided address is for {}.",
                    miner_address.network_kind(),
                ),
                // `Regtest` uses `Testnet` transparent addresses.
                network_kind @ (NetworkKind::Testnet | NetworkKind::Regtest) => assert_eq!(
                    miner_address.network_kind(),
                    NetworkKind::Testnet,
                    "Incorrect config: Zebra is configured to run on a {network_kind} network, \
                    which implies the configured mining address needs to be for Testnet, \
                    but the provided address is for {}.",
                    miner_address.network_kind(),
                ),
            }
        }

        // A limit on the configured extra coinbase data, regardless of the current block height.
        // This is different from the consensus rule, which limits the total height + data.
        const EXTRA_COINBASE_DATA_LIMIT: usize =
            MAX_COINBASE_DATA_LEN - MAX_COINBASE_HEIGHT_DATA_LEN;

        let debug_like_zcashd = mining_config.debug_like_zcashd;

        // Hex-decode to bytes if possible, otherwise UTF-8 encode to bytes.
        let extra_coinbase_data = mining_config.extra_coinbase_data.unwrap_or_else(|| {
            if debug_like_zcashd {
                ""
            } else {
                EXTRA_ZEBRA_COINBASE_DATA
            }
            .to_string()
        });
        let extra_coinbase_data = hex::decode(&extra_coinbase_data)
            .unwrap_or_else(|_error| extra_coinbase_data.as_bytes().to_vec());

        assert!(
            extra_coinbase_data.len() <= EXTRA_COINBASE_DATA_LIMIT,
            "extra coinbase data is {} bytes, but Zebra's limit is {}.\n\
             Configure mining.extra_coinbase_data with a shorter string",
            extra_coinbase_data.len(),
            EXTRA_COINBASE_DATA_LIMIT,
        );

        Self {
            network: network.clone(),
            miner_address: mining_config.miner_address,
            extra_coinbase_data,
            debug_like_zcashd,
            mempool,
            state,
            latest_chain_tip,
            block_verifier_router,
            sync_status,
            address_book,
            mined_block_sender: mined_block_sender
                .unwrap_or(submit_block::SubmitBlockChannel::default().sender()),
        }
    }
}

#[async_trait]
impl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook> GetBlockTemplateRpcServer
    for GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>
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
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    fn get_block_count(&self) -> Result<u32> {
        best_chain_tip_height(&self.latest_chain_tip).map(|height| height.0)
    }

    async fn get_block_hash(&self, index: i32) -> Result<GetBlockHash> {
        let mut state = self.state.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        // TODO: look up this height as part of the state request?
        let tip_height = best_chain_tip_height(&latest_chain_tip)?;

        let height = height_from_signed_int(index, tip_height)?;

        let request = zebra_state::ReadRequest::BestChainBlockHash(height);
        let response = state
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
        let miner_address = self.miner_address.clone();
        let debug_like_zcashd = self.debug_like_zcashd;
        let extra_coinbase_data = self.extra_coinbase_data.clone();

        // Clone Services
        let mempool = self.mempool.clone();
        let mut latest_chain_tip = self.latest_chain_tip.clone();
        let sync_status = self.sync_status.clone();
        let state = self.state.clone();

        if let Some(HexData(block_proposal_bytes)) = parameters
            .as_ref()
            .and_then(get_block_template::JsonParameters::block_proposal_data)
        {
            return validate_block_proposal(
                self.block_verifier_router.clone(),
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

        // - One-off checks

        // Check config and parameters.
        // These checks always have the same result during long polling.
        let miner_address = check_miner_address(miner_address)?;

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
            check_synced_to_tip(&network, latest_chain_tip.clone(), sync_status.clone())?;
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
            } = fetch_state_tip_and_local_time(state.clone()).await?;

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
                fetch_mempool_transactions(mempool.clone(), tip_hash)
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
            let wait_for_mempool_request = tokio::time::sleep(Duration::from_secs(
                GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
            ));

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
                        GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
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
                                    GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
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
        let mempool_txs = zip317::select_mempool_transactions(
            &network,
            next_block_height,
            &miner_address,
            mempool_txs,
            mempool_tx_deps,
            debug_like_zcashd,
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
            debug_like_zcashd,
            extra_coinbase_data,
        );

        Ok(response.into())
    }

    async fn submit_block(
        &self,
        HexData(block_bytes): HexData,
        _parameters: Option<submit_block::JsonParameters>,
    ) -> Result<submit_block::Response> {
        let mut block_verifier_router = self.block_verifier_router.clone();

        let block: Block = match block_bytes.zcash_deserialize_into() {
            Ok(block_bytes) => block_bytes,
            Err(error) => {
                tracing::info!(?error, "submit block failed: block bytes could not be deserialized into a structurally valid block");

                return Ok(submit_block::ErrorResponse::Rejected.into());
            }
        };

        let block_height = block
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
            Ok(block_hash) => {
                tracing::info!(?block_hash, ?block_height, "submit block accepted");

                self.mined_block_sender
                    .send((block_hash, block_height))
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
                    ?block_height,
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
        let mut state = self.state.clone();

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
            let response: zebra_state::ReadResponse = state
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
        let mut num_blocks = num_blocks.unwrap_or(DEFAULT_SOLUTION_RATE_WINDOW_SIZE);
        // But if it is 0 or negative, it uses the proof of work averaging window.
        if num_blocks < 1 {
            num_blocks = i32::try_from(POW_AVERAGING_WINDOW).expect("fits in i32");
        }
        let num_blocks =
            usize::try_from(num_blocks).expect("just checked for negatives, i32 fits in usize");

        // Default height is the tip height if not supplied. Negative values also mean the tip
        // height. Since negative values aren't valid heights, we can just use the conversion.
        let height = height.and_then(|height| height.try_into_height().ok());

        let mut state = self.state.clone();

        let request = ReadRequest::SolutionRate { num_blocks, height };

        let response = state
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
            .map(PeerInfo::new)
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
    ) -> Result<types::z_validate_address::Response> {
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

        let is_post_nu6 = NetworkUpgrade::current(&network, height) >= NetworkUpgrade::Nu6;

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
                        FundingStream::new(is_post_nu6, receiver, value, address)
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
        chain_tip_difficulty(self.network.clone(), self.state.clone(), false).await
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

    async fn generate(&self, num_blocks: u32) -> Result<Vec<GetBlockHash>> {
        let rpc: GetBlockTemplateRpcImpl<
            Mempool,
            State,
            Tip,
            BlockVerifierRouter,
            SyncStatus,
            AddressBook,
        > = self.clone();
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
                TimeSource::CurTime,
                NetworkUpgrade::current(&network, Height(block_template.height)),
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
}

// Put support functions in a submodule, to keep this file small.
