//! Types and functions for the `getblocktemplate` RPC.

pub mod constants;
pub mod parameters;
pub mod proposal;
pub mod zip317;

pub use constants::{
    CAPABILITIES_FIELD, DEFAULT_SOLUTION_RATE_WINDOW_SIZE,
    MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP, MEMPOOL_LONG_POLL_INTERVAL, MUTABLE_FIELD,
    NONCE_RANGE_FIELD, NOT_SYNCED_ERROR_CODE, ZCASHD_FUNDING_STREAM_ORDER,
};
pub use parameters::{GetBlockTemplateRequestMode, JsonParameters};
pub use proposal::{ProposalResponse, TimeSource};

use crate::{
    config,
    methods::{
        types::{
            default_roots::DefaultRoots, long_poll::LongPollId, submit_block,
            transaction::TransactionTemplate,
        },
        GetBlockHash,
    },
    server::error::OkOrError,
};
use jsonrpsee::core::RpcResult;
use jsonrpsee_types::{ErrorCode, ErrorObject};
use std::{collections::HashMap, fmt, iter, sync::Arc};
use tokio::sync::watch::{self, error::SendError};
use tower::{Service, ServiceExt};
use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::{
        self,
        merkle::{self, AuthDataRoot},
        Block, ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height,
        MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION,
    },
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::{subsidy::FundingStreamReceiver, Network, NetworkKind, NetworkUpgrade},
    serialization::{DateTime32, ZcashDeserializeInto},
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent::{
        self, EXTRA_ZEBRA_COINBASE_DATA, MAX_COINBASE_DATA_LEN, MAX_COINBASE_HEIGHT_DATA_LEN,
    },
    work::difficulty::{CompactDifficulty, ExpandedDifficulty},
};
use zebra_consensus::{
    block_subsidy, funding_stream_address, funding_stream_values, miner_subsidy, MAX_BLOCK_SIGOPS,
};
use zebra_node_services::mempool::{self, TransactionDependencies};
use zebra_state::GetBlockTemplateChainInfo;

/// An alias to indicate that a usize value represents the depth of in-block dependencies of a
/// transaction.
///
/// See the `dependencies_depth()` function in [`zip317`] for more details.
pub type InBlockTxDependenciesDepth = usize;

/// A serialized `getblocktemplate` RPC response in template mode.
///
/// This is the output of the `getblocktemplate` RPC in the default 'template' mode. See
/// [`ProposalResponse`] for the output in 'proposal' mode.
#[derive(Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockTemplate {
    /// The getblocktemplate RPC capabilities supported by Zebra.
    ///
    /// At the moment, Zebra does not support any of the extra capabilities from the specification:
    /// - `proposal`: <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
    /// - `longpoll`: <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    /// - `serverlist`: <https://en.bitcoin.it/wiki/BIP_0023#Logical_Services>
    ///
    /// By the above, Zebra will always return an empty vector here.
    pub capabilities: Vec<String>,

    /// The version of the block format.
    /// Always 4 for new Zcash blocks.
    pub version: u32,

    /// The hash of the previous block.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,

    /// The block commitment for the new block's header.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "lightclientroothash")]
    #[serde(with = "hex")]
    pub light_client_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "finalsaplingroothash")]
    #[serde(with = "hex")]
    pub final_sapling_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// The block header roots for [`GetBlockTemplate.transactions`].
    ///
    /// If the transactions in the block template are modified, these roots must be recalculated
    /// [according to the specification](https://zcash.github.io/rpc/getblocktemplate.html).
    #[serde(rename = "defaultroots")]
    pub default_roots: DefaultRoots,

    /// The non-coinbase transactions selected for this block template.
    pub transactions: Vec<TransactionTemplate<amount::NonNegative>>,

    /// The coinbase transaction generated from `transactions` and `height`.
    #[serde(rename = "coinbasetxn")]
    pub coinbase_txn: TransactionTemplate<amount::NegativeOrZero>,

    /// An ID that represents the chain tip and mempool contents for this template.
    #[serde(rename = "longpollid")]
    pub long_poll_id: LongPollId,

    /// The expected difficulty for the new block displayed in expanded form.
    #[serde(with = "hex")]
    pub target: ExpandedDifficulty,

    /// > For each block other than the genesis block, nTime MUST be strictly greater than
    /// > the median-time-past of that block.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    #[serde(rename = "mintime")]
    pub min_time: DateTime32,

    /// Hardcoded list of block fields the miner is allowed to change.
    pub mutable: Vec<String>,

    /// A range of valid nonces that goes from `u32::MIN` to `u32::MAX`.
    #[serde(rename = "noncerange")]
    pub nonce_range: String,

    /// Max legacy signature operations in the block.
    #[serde(rename = "sigoplimit")]
    pub sigop_limit: u64,

    /// Max block size in bytes
    #[serde(rename = "sizelimit")]
    pub size_limit: u64,

    /// > the current time as seen by the server (recommended for block time).
    /// > note this is not necessarily the system clock, and must fall within the mintime/maxtime rules
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Block_Template_Request>
    #[serde(rename = "curtime")]
    pub cur_time: DateTime32,

    /// The expected difficulty for the new block displayed in compact form.
    #[serde(with = "hex")]
    pub bits: CompactDifficulty,

    /// The height of the next block in the best chain.
    // Optional TODO: use Height type, but check that deserialized heights are within Height::MAX
    pub height: u32,

    /// > the maximum time allowed
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0023#Mutations>
    ///
    /// Zebra adjusts the minimum and current times for testnet minimum difficulty blocks,
    /// so we need to tell miners what the maximum valid time is.
    ///
    /// This field is not in `zcashd` or the Zcash RPC reference yet.
    ///
    /// Currently, some miners just use `min_time` or `cur_time`. Others calculate `max_time` from the
    /// fixed 90 minute consensus rule, or a smaller fixed interval (like 1000s).
    /// Some miners don't check the maximum time. This can cause invalid blocks after network downtime,
    /// a significant drop in the hash rate, or after the testnet minimum difficulty interval.
    #[serde(rename = "maxtime")]
    pub max_time: DateTime32,

    /// > only relevant for long poll responses:
    /// > indicates if work received prior to this response remains potentially valid (default)
    /// > and should have its shares submitted;
    /// > if false, the miner may wish to discard its share queue
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    ///
    /// This field is not in `zcashd` or the Zcash RPC reference yet.
    ///
    /// In Zebra, `submit_old` is `false` when the tip block changed or max time is reached,
    /// and `true` if only the mempool transactions have changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    #[serde(rename = "submitold")]
    pub submit_old: Option<bool>,
}

impl fmt::Debug for GetBlockTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A block with a lot of transactions can be extremely long in logs.
        let mut transactions_truncated = self.transactions.clone();
        if self.transactions.len() > 4 {
            // Remove transaction 3 onwards, but leave the last transaction
            let end = self.transactions.len() - 2;
            transactions_truncated.splice(3..=end, Vec::new());
        }

        f.debug_struct("GetBlockTemplate")
            .field("capabilities", &self.capabilities)
            .field("version", &self.version)
            .field("previous_block_hash", &self.previous_block_hash)
            .field("block_commitments_hash", &self.block_commitments_hash)
            .field("light_client_root_hash", &self.light_client_root_hash)
            .field("final_sapling_root_hash", &self.final_sapling_root_hash)
            .field("default_roots", &self.default_roots)
            .field("transaction_count", &self.transactions.len())
            .field("transactions", &transactions_truncated)
            .field("coinbase_txn", &self.coinbase_txn)
            .field("long_poll_id", &self.long_poll_id)
            .field("target", &self.target)
            .field("min_time", &self.min_time)
            .field("mutable", &self.mutable)
            .field("nonce_range", &self.nonce_range)
            .field("sigop_limit", &self.sigop_limit)
            .field("size_limit", &self.size_limit)
            .field("cur_time", &self.cur_time)
            .field("bits", &self.bits)
            .field("height", &self.height)
            .field("max_time", &self.max_time)
            .field("submit_old", &self.submit_old)
            .finish()
    }
}

impl GetBlockTemplate {
    /// Returns a `Vec` of capabilities supported by the `getblocktemplate` RPC
    pub fn capabilities() -> Vec<String> {
        CAPABILITIES_FIELD.iter().map(ToString::to_string).collect()
    }

    /// Returns a new [`GetBlockTemplate`] struct, based on the supplied arguments and defaults.
    ///
    /// The result of this method only depends on the supplied arguments and constants.
    ///
    /// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
    /// in the `getblocktemplate` RPC.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network: &Network,
        miner_address: &transparent::Address,
        chain_tip_and_local_time: &GetBlockTemplateChainInfo,
        long_poll_id: LongPollId,
        #[cfg(not(test))] mempool_txs: Vec<VerifiedUnminedTx>,
        #[cfg(test)] mempool_txs: Vec<(InBlockTxDependenciesDepth, VerifiedUnminedTx)>,
        submit_old: Option<bool>,
        like_zcashd: bool,
        extra_coinbase_data: Vec<u8>,
    ) -> Self {
        // Calculate the next block height.
        let next_block_height =
            (chain_tip_and_local_time.tip_height + 1).expect("tip is far below Height::MAX");

        // Convert transactions into TransactionTemplates
        #[cfg(not(test))]
        let (mempool_tx_templates, mempool_txs): (Vec<_>, Vec<_>) =
            mempool_txs.into_iter().map(|tx| ((&tx).into(), tx)).unzip();

        // Transaction selection returns transactions in an arbitrary order,
        // but Zebra's snapshot tests expect the same order every time.
        //
        // # Correctness
        //
        // Transactions that spend outputs created in the same block must appear
        // after the transactions that create those outputs.
        #[cfg(test)]
        let (mempool_tx_templates, mempool_txs): (Vec<_>, Vec<_>) = {
            let mut mempool_txs_with_templates: Vec<(
                InBlockTxDependenciesDepth,
                TransactionTemplate<amount::NonNegative>,
                VerifiedUnminedTx,
            )> = mempool_txs
                .into_iter()
                .map(|(min_tx_index, tx)| (min_tx_index, (&tx).into(), tx))
                .collect();

            if like_zcashd {
                // Sort in serialized data order, excluding the length byte.
                // `zcashd` sometimes seems to do this, but other times the order is arbitrary.
                mempool_txs_with_templates.sort_by_key(|(min_tx_index, tx_template, _tx)| {
                    (*min_tx_index, tx_template.data.clone())
                });
            } else {
                // Sort by hash, this is faster.
                mempool_txs_with_templates.sort_by_key(|(min_tx_index, tx_template, _tx)| {
                    (*min_tx_index, tx_template.hash.bytes_in_display_order())
                });
            }
            mempool_txs_with_templates
                .into_iter()
                .map(|(_, template, tx)| (template, tx))
                .unzip()
        };

        // Generate the coinbase transaction and default roots
        //
        // TODO: move expensive root, hash, and tree cryptography to a rayon thread?
        let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
            network,
            next_block_height,
            miner_address,
            &mempool_txs,
            chain_tip_and_local_time.chain_history_root,
            like_zcashd,
            extra_coinbase_data,
        );

        // Convert difficulty
        let target = chain_tip_and_local_time
            .expected_difficulty
            .to_expanded()
            .expect("state always returns a valid difficulty value");

        // Convert default values
        let capabilities: Vec<String> = Self::capabilities();
        let mutable: Vec<String> = MUTABLE_FIELD.iter().map(ToString::to_string).collect();

        tracing::debug!(
            selected_txs = ?mempool_txs
                .iter()
                .map(|tx| (tx.transaction.id.mined_id(), tx.unpaid_actions))
                .collect::<Vec<_>>(),
            "creating template ... "
        );

        GetBlockTemplate {
            capabilities,

            version: ZCASH_BLOCK_VERSION,

            previous_block_hash: GetBlockHash(chain_tip_and_local_time.tip_hash),
            block_commitments_hash: default_roots.block_commitments_hash,
            light_client_root_hash: default_roots.block_commitments_hash,
            final_sapling_root_hash: default_roots.block_commitments_hash,
            default_roots,

            transactions: mempool_tx_templates,

            coinbase_txn,

            long_poll_id,

            target,

            min_time: chain_tip_and_local_time.min_time,

            mutable,

            nonce_range: NONCE_RANGE_FIELD.to_string(),

            sigop_limit: MAX_BLOCK_SIGOPS,

            size_limit: MAX_BLOCK_BYTES,

            cur_time: chain_tip_and_local_time.cur_time,

            bits: chain_tip_and_local_time.expected_difficulty,

            height: next_block_height.0,

            max_time: chain_tip_and_local_time.max_time,

            submit_old,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
/// A `getblocktemplate` RPC response.
pub enum Response {
    /// `getblocktemplate` RPC request in template mode.
    TemplateMode(Box<GetBlockTemplate>),

    /// `getblocktemplate` RPC request in proposal mode.
    ProposalMode(ProposalResponse),
}

impl Response {
    /// Returns the inner template, if the response is in template mode.
    pub fn try_into_template(self) -> Option<GetBlockTemplate> {
        match self {
            Response::TemplateMode(template) => Some(*template),
            Response::ProposalMode(_) => None,
        }
    }

    /// Returns the inner proposal, if the response is in proposal mode.
    pub fn try_into_proposal(self) -> Option<ProposalResponse> {
        match self {
            Response::TemplateMode(_) => None,
            Response::ProposalMode(proposal) => Some(proposal),
        }
    }
}

///  Handler for the `getblocktemplate` RPC.
#[derive(Clone)]
pub struct GetBlockTemplateHandler<BlockVerifierRouter, SyncStatus>
where
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    /// The configured miner address for this RPC service.
    ///
    /// Zebra currently only supports transparent addresses.
    miner_address: Option<transparent::Address>,

    /// Extra data to include in coinbase transaction inputs.
    /// Limited to around 95 bytes by the consensus rules.
    extra_coinbase_data: Vec<u8>,

    /// The chain verifier, used for submitting blocks.
    block_verifier_router: BlockVerifierRouter,

    /// The chain sync status, used for checking if Zebra is likely close to the network chain tip.
    sync_status: SyncStatus,

    /// A channel to send successful block submissions to the block gossip task,
    /// so they can be advertised to peers.
    mined_block_sender: watch::Sender<(block::Hash, block::Height)>,
}

impl<BlockVerifierRouter, SyncStatus> GetBlockTemplateHandler<BlockVerifierRouter, SyncStatus>
where
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    /// Creates a new [`GetBlockTemplateHandler`].
    ///
    /// # Panics
    ///
    /// - If the provided `mining_config` is not valid.
    pub fn new(
        net: &Network,
        conf: config::mining::Config,
        block_verifier_router: BlockVerifierRouter,
        sync_status: SyncStatus,
        mined_block_sender: Option<watch::Sender<(block::Hash, block::Height)>>,
    ) -> Self {
        // Prevent loss of miner funds due to an unsupported or incorrect address type.
        if let Some(miner_address) = conf.miner_address.clone() {
            match net.kind() {
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

        let debug_like_zcashd = conf.debug_like_zcashd;

        // Hex-decode to bytes if possible, otherwise UTF-8 encode to bytes.
        let extra_coinbase_data = conf.extra_coinbase_data.unwrap_or_else(|| {
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
            miner_address: conf.miner_address,
            extra_coinbase_data,
            block_verifier_router,
            sync_status,
            mined_block_sender: mined_block_sender
                .unwrap_or(submit_block::SubmitBlockChannel::default().sender()),
        }
    }

    /// Returns the miner's address.
    pub fn miner_address(&self) -> Option<transparent::Address> {
        self.miner_address.clone()
    }

    /// Returns the extra coinbase data.
    pub fn extra_coinbase_data(&self) -> Vec<u8> {
        self.extra_coinbase_data.clone()
    }

    /// Returns the sync status.
    pub fn sync_status(&self) -> SyncStatus {
        self.sync_status.clone()
    }

    /// Returns the block verifier router.
    pub fn block_verifier_router(&self) -> BlockVerifierRouter {
        self.block_verifier_router.clone()
    }

    /// Advertises the mined block.
    pub fn advertise_mined_block(
        &self,
        block: block::Hash,
        height: block::Height,
    ) -> Result<(), SendError<(block::Hash, block::Height)>> {
        self.mined_block_sender.send((block, height))
    }
}

impl<BlockVerifierRouter, SyncStatus> fmt::Debug
    for GetBlockTemplateHandler<BlockVerifierRouter, SyncStatus>
where
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Skip fields without debug impls
        f.debug_struct("GetBlockTemplateRpcImpl")
            .field("miner_address", &self.miner_address)
            .field("extra_coinbase_data", &self.extra_coinbase_data)
            .finish()
    }
}

// - Parameter checks

/// Checks that `data` is omitted in `Template` mode or provided in `Proposal` mode,
///
/// Returns an error if there's a mismatch between the mode and whether `data` is provided.
pub fn check_parameters(parameters: &Option<JsonParameters>) -> RpcResult<()> {
    let Some(parameters) = parameters else {
        return Ok(());
    };

    match parameters {
        JsonParameters {
            mode: GetBlockTemplateRequestMode::Template,
            data: None,
            ..
        }
        | JsonParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            data: Some(_),
            ..
        } => Ok(()),

        JsonParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            data: None,
            ..
        } => Err(ErrorObject::borrowed(
            ErrorCode::InvalidParams.code(),
            "\"data\" parameter must be \
                provided in \"proposal\" mode",
            None,
        )),

        JsonParameters {
            mode: GetBlockTemplateRequestMode::Template,
            data: Some(_),
            ..
        } => Err(ErrorObject::borrowed(
            ErrorCode::InvalidParams.code(),
            "\"data\" parameter must be \
                omitted in \"template\" mode",
            None,
        )),
    }
}

/// Returns the miner address, or an error if it is invalid.
pub fn check_miner_address(
    miner_address: Option<transparent::Address>,
) -> RpcResult<transparent::Address> {
    miner_address.ok_or_misc_error(
        "set `mining.miner_address` in `zebrad.toml` to a transparent address".to_string(),
    )
}

/// Attempts to validate block proposal against all of the server's
/// usual acceptance rules (except proof-of-work).
///
/// Returns a `getblocktemplate` [`Response`].
pub async fn validate_block_proposal<BlockVerifierRouter, Tip, SyncStatus>(
    mut block_verifier_router: BlockVerifierRouter,
    block_proposal_bytes: Vec<u8>,
    network: Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
) -> RpcResult<Response>
where
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    check_synced_to_tip(&network, latest_chain_tip, sync_status)?;

    let block: Block = match block_proposal_bytes.zcash_deserialize_into() {
        Ok(block) => block,
        Err(parse_error) => {
            tracing::info!(
                ?parse_error,
                "error response from block parser in CheckProposal request"
            );

            return Ok(
                ProposalResponse::rejected("invalid proposal format", parse_error.into()).into(),
            );
        }
    };

    let block_verifier_router_response = block_verifier_router
        .ready()
        .await
        .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?
        .call(zebra_consensus::Request::CheckProposal(Arc::new(block)))
        .await;

    Ok(block_verifier_router_response
        .map(|_hash| ProposalResponse::Valid)
        .unwrap_or_else(|verify_chain_error| {
            tracing::info!(
                ?verify_chain_error,
                "error response from block_verifier_router in CheckProposal request"
            );

            ProposalResponse::rejected("invalid proposal", verify_chain_error)
        })
        .into())
}

// - State and syncer checks

/// Returns an error if Zebra is not synced to the consensus chain tip.
/// Returns early with `Ok(())` if Proof-of-Work is disabled on the provided `network`.
/// This error might be incorrect if the local clock is skewed.
pub fn check_synced_to_tip<Tip, SyncStatus>(
    network: &Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
) -> RpcResult<()>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    if network.is_a_test_network() {
        return Ok(());
    }

    // The tip estimate may not be the same as the one coming from the state
    // but this is ok for an estimate
    let (estimated_distance_to_chain_tip, local_tip_height) = latest_chain_tip
        .estimate_distance_to_network_chain_tip(network)
        .ok_or_misc_error("no chain tip available yet")?;

    if !sync_status.is_close_to_tip()
        || estimated_distance_to_chain_tip > MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP
    {
        tracing::info!(
            ?estimated_distance_to_chain_tip,
            ?local_tip_height,
            "Zebra has not synced to the chain tip. \
             Hint: check your network connection, clock, and time zone settings."
        );

        return Err(ErrorObject::borrowed(
            NOT_SYNCED_ERROR_CODE.code(),
            "Zebra has not synced to the chain tip, \
                 estimated distance: {estimated_distance_to_chain_tip:?}, \
                 local tip: {local_tip_height:?}. \
                 Hint: check your network connection, clock, and time zone settings.",
            None,
        ));
    }

    Ok(())
}

// - State and mempool data fetches

/// Returns the state data for the block template.
///
/// You should call `check_synced_to_tip()` before calling this function.
/// If the state does not have enough blocks, returns an error.
pub async fn fetch_state_tip_and_local_time<State>(
    state: State,
) -> RpcResult<GetBlockTemplateChainInfo>
where
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    let request = zebra_state::ReadRequest::ChainInfo;
    let response = state
        .oneshot(request.clone())
        .await
        .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?;

    let chain_info = match response {
        zebra_state::ReadResponse::ChainInfo(chain_info) => chain_info,
        _ => unreachable!("incorrect response to {request:?}"),
    };

    Ok(chain_info)
}

/// Returns the transactions that are currently in `mempool`, or None if the
/// `last_seen_tip_hash` from the mempool response doesn't match the tip hash from the state.
///
/// You should call `check_synced_to_tip()` before calling this function.
/// If the mempool is inactive because Zebra is not synced to the tip, returns no transactions.
pub async fn fetch_mempool_transactions<Mempool>(
    mempool: Mempool,
    chain_tip_hash: block::Hash,
) -> RpcResult<Option<(Vec<VerifiedUnminedTx>, TransactionDependencies)>>
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
        .map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))?;

    // TODO: Order transactions in block templates based on their dependencies

    let mempool::Response::FullTransactions {
        transactions,
        transaction_dependencies,
        last_seen_tip_hash,
    } = response
    else {
        unreachable!("unmatched response to a mempool::FullTransactions request")
    };

    // Check that the mempool and state were in sync when we made the requests
    Ok((last_seen_tip_hash == chain_tip_hash).then_some((transactions, transaction_dependencies)))
}

// - Response processing

/// Generates and returns the coinbase transaction and default roots.
///
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
pub fn generate_coinbase_and_roots(
    network: &Network,
    block_template_height: Height,
    miner_address: &transparent::Address,
    mempool_txs: &[VerifiedUnminedTx],
    chain_history_root: Option<ChainHistoryMmrRootHash>,
    like_zcashd: bool,
    extra_coinbase_data: Vec<u8>,
) -> (TransactionTemplate<NegativeOrZero>, DefaultRoots) {
    // Generate the coinbase transaction
    let miner_fee = calculate_miner_fee(mempool_txs);
    let coinbase_txn = generate_coinbase_transaction(
        network,
        block_template_height,
        miner_address,
        miner_fee,
        like_zcashd,
        extra_coinbase_data,
    );

    // Calculate block default roots
    //
    // TODO: move expensive root, hash, and tree cryptography to a rayon thread?
    let chain_history_root = chain_history_root
        .or_else(|| {
            (NetworkUpgrade::Heartwood.activation_height(network) == Some(block_template_height))
                .then_some([0; 32].into())
        })
        .expect("history tree can't be empty");
    let default_roots =
        calculate_default_root_hashes(&coinbase_txn, mempool_txs, chain_history_root);

    let coinbase_txn = TransactionTemplate::from_coinbase(&coinbase_txn, miner_fee);

    (coinbase_txn, default_roots)
}

// - Coinbase transaction processing

/// Returns a coinbase transaction for the supplied parameters.
///
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
pub fn generate_coinbase_transaction(
    network: &Network,
    height: Height,
    miner_address: &transparent::Address,
    miner_fee: Amount<NonNegative>,
    like_zcashd: bool,
    extra_coinbase_data: Vec<u8>,
) -> UnminedTx {
    let outputs = standard_coinbase_outputs(network, height, miner_address, miner_fee, like_zcashd);

    if like_zcashd {
        Transaction::new_v4_coinbase(network, height, outputs, like_zcashd, extra_coinbase_data)
            .into()
    } else {
        Transaction::new_v5_coinbase(network, height, outputs, extra_coinbase_data).into()
    }
}

/// Returns the total miner fee for `mempool_txs`.
pub fn calculate_miner_fee(mempool_txs: &[VerifiedUnminedTx]) -> Amount<NonNegative> {
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
///
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
pub fn standard_coinbase_outputs(
    network: &Network,
    height: Height,
    miner_address: &transparent::Address,
    miner_fee: Amount<NonNegative>,
    like_zcashd: bool,
) -> Vec<(Amount<NonNegative>, transparent::Script)> {
    let expected_block_subsidy = block_subsidy(height, network).expect("valid block subsidy");
    let funding_streams = funding_stream_values(height, network, expected_block_subsidy)
        .expect("funding stream value calculations are valid for reasonable chain heights");

    // Optional TODO: move this into a zebra_consensus function?
    let funding_streams: HashMap<
        FundingStreamReceiver,
        (Amount<NonNegative>, &transparent::Address),
    > = funding_streams
        .into_iter()
        .filter_map(|(receiver, amount)| {
            Some((
                receiver,
                (amount, funding_stream_address(height, network, receiver)?),
            ))
        })
        .collect();

    let miner_reward = miner_subsidy(height, network, expected_block_subsidy)
        .expect("reward calculations are valid for reasonable chain heights")
        + miner_fee;
    let miner_reward =
        miner_reward.expect("reward calculations are valid for reasonable chain heights");

    combine_coinbase_outputs(funding_streams, miner_address, miner_reward, like_zcashd)
}

/// Combine the miner reward and funding streams into a list of coinbase amounts and addresses.
///
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
fn combine_coinbase_outputs(
    funding_streams: HashMap<FundingStreamReceiver, (Amount<NonNegative>, &transparent::Address)>,
    miner_address: &transparent::Address,
    miner_reward: Amount<NonNegative>,
    like_zcashd: bool,
) -> Vec<(Amount<NonNegative>, transparent::Script)> {
    // Collect all the funding streams and convert them to outputs.
    let funding_streams_outputs: Vec<(Amount<NonNegative>, &transparent::Address)> =
        funding_streams
            .into_iter()
            .map(|(_receiver, (amount, address))| (amount, address))
            .collect();

    let mut coinbase_outputs: Vec<(Amount<NonNegative>, transparent::Script)> =
        funding_streams_outputs
            .iter()
            .map(|(amount, address)| (*amount, address.create_script_from_address()))
            .collect();

    // The HashMap returns funding streams in an arbitrary order,
    // but Zebra's snapshot tests expect the same order every time.
    if like_zcashd {
        // zcashd sorts outputs in serialized data order, excluding the length field
        coinbase_outputs.sort_by_key(|(_amount, script)| script.clone());

        // The miner reward is always the first output independent of the sort order
        coinbase_outputs.insert(
            0,
            (miner_reward, miner_address.create_script_from_address()),
        );
    } else {
        // Unlike zcashd, in Zebra the miner reward is part of the sorting
        coinbase_outputs.push((miner_reward, miner_address.create_script_from_address()));

        // Zebra sorts by amount then script.
        //
        // Since the sort is stable, equal amounts will remain sorted by script.
        coinbase_outputs.sort_by_key(|(_amount, script)| script.clone());
        coinbase_outputs.sort_by_key(|(amount, _script)| *amount);
    }

    coinbase_outputs
}

// - Transaction roots processing

/// Returns the default block roots for the supplied coinbase and mempool transactions,
/// and the supplied history tree.
///
/// This function runs expensive cryptographic operations.
pub fn calculate_default_root_hashes(
    coinbase_txn: &UnminedTx,
    mempool_txs: &[VerifiedUnminedTx],
    chain_history_root: ChainHistoryMmrRootHash,
) -> DefaultRoots {
    let (merkle_root, auth_data_root) = calculate_transaction_roots(coinbase_txn, mempool_txs);

    let block_commitments_hash = if chain_history_root == [0; 32].into() {
        [0; 32].into()
    } else {
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &chain_history_root,
            &auth_data_root,
        )
    };

    DefaultRoots {
        merkle_root,
        chain_history_root,
        auth_data_root,
        block_commitments_hash,
    }
}

/// Returns the transaction effecting and authorizing roots
/// for `coinbase_txn` and `mempool_txs`, which are used in the block header.
//
// TODO: should this be spawned into a cryptographic operations pool?
//       (it would only matter if there were a lot of small transactions in a block)
pub fn calculate_transaction_roots(
    coinbase_txn: &UnminedTx,
    mempool_txs: &[VerifiedUnminedTx],
) -> (merkle::Root, AuthDataRoot) {
    let block_transactions =
        || iter::once(coinbase_txn).chain(mempool_txs.iter().map(|tx| &tx.transaction));

    let merkle_root = block_transactions().cloned().collect();
    let auth_data_root = block_transactions().cloned().collect();

    (merkle_root, auth_data_root)
}
