//! Support functions for the `get_block_template()` RPC.

use std::iter;

use jsonrpc_core::{Error, ErrorCode, Result};
use tower::{Service, ServiceExt};

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::{
        merkle::{self, AuthDataRoot},
        Height,
    },
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    parameters::Network,
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::{funding_stream_address, funding_stream_values, miner_subsidy};
use zebra_node_services::mempool;
use zebra_state::GetBlockTemplateChainInfo;

use crate::methods::get_block_template_rpcs::{
    constants::{MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP, NOT_SYNCED_ERROR_CODE},
    types::get_block_template,
};

pub use crate::methods::get_block_template_rpcs::types::get_block_template::*;

// - Parameter checks

/// Returns an error if the get block template RPC `parameters` are invalid.
pub fn check_block_template_parameters(
    parameters: get_block_template::JsonParameters,
) -> Result<()> {
    if parameters.data.is_some() || parameters.mode == GetBlockTemplateRequestMode::Proposal {
        return Err(Error {
            code: ErrorCode::InvalidParams,
            message: "\"proposal\" mode is currently unsupported by Zebra".to_string(),
            data: None,
        });
    }

    Ok(())
}

/// Returns the miner address, or an error if it is invalid.
pub fn check_address(miner_address: Option<transparent::Address>) -> Result<transparent::Address> {
    miner_address.ok_or_else(|| Error {
        code: ErrorCode::ServerError(0),
        message: "configure mining.miner_address in zebrad.toml \
                  with a transparent address"
            .to_string(),
        data: None,
    })
}

// - State and syncer checks

/// Returns an error if Zebra is not synced to the consensus chain tip.
/// This error might be incorrect if the local clock is skewed.
pub fn check_synced_to_tip<Tip, SyncStatus>(
    network: Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
) -> Result<()>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // The tip estimate may not be the same as the one coming from the state
    // but this is ok for an estimate
    let (estimated_distance_to_chain_tip, local_tip_height) = latest_chain_tip
        .estimate_distance_to_network_chain_tip(network)
        .ok_or_else(|| Error {
            code: ErrorCode::ServerError(0),
            message: "No Chain tip available yet".to_string(),
            data: None,
        })?;

    if !sync_status.is_close_to_tip()
        || estimated_distance_to_chain_tip > MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP
    {
        tracing::info!(
            estimated_distance_to_chain_tip,
            ?local_tip_height,
            "Zebra has not synced to the chain tip. \
             Hint: check your network connection, clock, and time zone settings."
        );

        return Err(Error {
            code: NOT_SYNCED_ERROR_CODE,
            message: format!(
                "Zebra has not synced to the chain tip, \
                 estimated distance: {estimated_distance_to_chain_tip}, \
                 local tip: {local_tip_height:?}. \
                 Hint: check your network connection, clock, and time zone settings."
            ),
            data: None,
        });
    }

    Ok(())
}

// - State and mempool data fetches

/// Returns the state data for the block template.
///
/// # Panics
///
/// If `check_synced_to_tip()` would return an error
/// because there are not enough blocks in the state.
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
    let response = state.oneshot(request).await.map_err(|error| Error {
        code: ErrorCode::ServerError(0),
        message: error.to_string(),
        data: None,
    })?;

    let chain_info = match response {
        zebra_state::ReadResponse::ChainInfo(chain_info) => chain_info,
        _ => unreachable!("must call check_synced_to_tip() before `GetBlockTemplateChainInfo`"),
    };

    Ok(chain_info)
}

/// Returns the transactions that are currently in `mempool`.
///
/// You should call `check_synced_to_tip()` before calling this function.
/// If the mempool is inactive because Zebra is not synced to the tip, returns no transactions.
//
// TODO: return an error if the mempool is inactive?
pub async fn fetch_mempool_transactions<Mempool>(mempool: Mempool) -> Result<Vec<VerifiedUnminedTx>>
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
        Ok(transactions)
    } else {
        unreachable!("unmatched response to a mempool::FullTransactions request")
    }
}

// - Coinbase transaction processing

/// Returns a coinbase transaction for the supplied parameters.
pub fn generate_coinbase_transaction(
    network: Network,
    height: Height,
    miner_address: transparent::Address,
    miner_fee: Amount<NonNegative>,
) -> UnminedTx {
    let outputs = standard_coinbase_outputs(network, height, miner_address, miner_fee);

    Transaction::new_v5_coinbase(network, height, outputs).into()
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
        .map(|(amount, address)| (*amount, address.create_script_from_address()))
        .collect()
}

// - Transaction roots processing

/// Returns the transaction effecting and authorizing roots
/// for `coinbase_tx` and `mempool_txs`, which are used in the block header.
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
