//! Support functions for the `get_block_template()` RPC.

use std::{collections::HashMap, iter, sync::Arc};

use jsonrpsee::core::RpcResult as Result;
use jsonrpsee_types::{ErrorCode, ErrorObject};
use tower::{Service, ServiceExt};

use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::{
        self,
        merkle::{self, AuthDataRoot},
        Block, ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height,
    },
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::{
        subsidy::{block_subsidy, funding_stream_values, miner_subsidy, FundingStreamReceiver},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::funding_stream_address;
use zebra_node_services::mempool::{self, TransactionDependencies};
use zebra_state::GetBlockTemplateChainInfo;

use crate::{
    methods::get_block_template_rpcs::{
        constants::{MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP, NOT_SYNCED_ERROR_CODE},
        types::{default_roots::DefaultRoots, transaction::TransactionTemplate},
    },
    server::error::OkOrError,
};

pub use crate::methods::get_block_template_rpcs::types::get_block_template::*;

// - Parameter checks

/// Checks that `data` is omitted in `Template` mode or provided in `Proposal` mode,
///
/// Returns an error if there's a mismatch between the mode and whether `data` is provided.
pub fn check_parameters(parameters: &Option<JsonParameters>) -> Result<()> {
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
) -> Result<transparent::Address> {
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
) -> Result<Response>
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
) -> Result<()>
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
) -> Result<GetBlockTemplateChainInfo>
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
) -> Result<Option<(Vec<VerifiedUnminedTx>, TransactionDependencies)>>
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
    history_tree: Arc<zebra_chain::history_tree::HistoryTree>,
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
    let chain_history_root = history_tree
        .hash()
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
