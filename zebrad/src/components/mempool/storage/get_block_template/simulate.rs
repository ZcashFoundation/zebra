use tokio::{runtime::Handle, task::block_in_place};

use zebra_chain::{parameters::NetworkUpgrade, serialization::ZcashSerialize};

use zebra_rpc::methods::types::{
    get_block_template::{
        fetch_state_tip_and_local_time, proposal::proposal_block_from_template,
        validate_block_proposal, GetBlockTemplate, GetBlockTemplateHandler, Response,
    },
    long_poll::LongPollInput,
};

use crate::components::mempool::{storage::verified_set::VerifiedSet, GetblockTemplateData};

/// An error that can occur while simulating a block template.
pub enum SimulateBlockTemplateError {
    /// We have no state to verify the block template against.
    NoState,

    /// Invalid miner address provided in the mining configuration.
    InvalidMinerAddress,

    /// Not enough blocks in the state to fetch the chain tip and local time.
    NotenoughBlocksInState,

    /// Invalid tip height or hash.
    InvalidTip,

    /// Error while serializing proposal block from template.
    BlockSerializationError,

    /// Error while validating the block proposal.
    InvalidBlockProposalResponse,

    /// An unexpected response type was received during validation.
    UnexpectedValidationResponse,
}

/// Simulate a block template against the current mempool.
///
/// This is a synchronous wrapper around the asynchronous `async_verify_block_template` function.
///
/// Returns `Ok(true)` if the block template is valid for the current mempool,
/// `Ok(false)` if it is invalid, or an error if something went wrong.
///
/// No panics should occur during the simulation.
pub fn verify_block_template(
    mempool: &VerifiedSet,
    verify_block_proposal_data: GetblockTemplateData,
) -> Result<bool, SimulateBlockTemplateError> {
    block_in_place(|| {
        Handle::current().block_on(async {
            async_verify_block_template(mempool, verify_block_proposal_data).await
        })
    })
}

/// Asynchronously verify a block template against the current mempool.
pub async fn async_verify_block_template(
    mempool: &VerifiedSet,
    verify_block_proposal_data: GetblockTemplateData,
) -> Result<bool, SimulateBlockTemplateError> {
    let state = verify_block_proposal_data
        .state
        .ok_or(SimulateBlockTemplateError::NoState)?;
    let network = verify_block_proposal_data.network.clone();
    let mining_config = verify_block_proposal_data.mining_config.clone();
    let miner_address = mining_config
        .clone()
        .miner_address
        .ok_or(SimulateBlockTemplateError::InvalidMinerAddress)?;
    let block_verifier_router = verify_block_proposal_data.block_verifier_router.clone();
    let sync_status = verify_block_proposal_data.sync_status.clone();
    let latest_chain_tip = verify_block_proposal_data.latest_chain_tip.clone();

    let chain_tip_and_local_time = fetch_state_tip_and_local_time(state)
        .await
        .map_err(|_| SimulateBlockTemplateError::NotenoughBlocksInState)?;

    let network_upgrade = NetworkUpgrade::current(
        &network,
        (chain_tip_and_local_time.tip_height + 1).ok_or(SimulateBlockTemplateError::InvalidTip)?,
    );

    let longpollid = LongPollInput::new(
        chain_tip_and_local_time.tip_height,
        chain_tip_and_local_time.tip_hash,
        chain_tip_and_local_time.max_time,
        mempool.transactions().values().map(|tx| tx.transaction.id),
    )
    .generate_id();

    let txs = mempool.transactions().values().cloned().collect();

    let response = GetBlockTemplate::new(
        &network,
        &miner_address,
        &chain_tip_and_local_time,
        longpollid,
        txs,
        None,
        true,
        vec![],
    );

    let block = proposal_block_from_template(&response, None, network_upgrade)
        .map_err(|_| SimulateBlockTemplateError::BlockSerializationError)?;

    let block_bytes = block
        .zcash_serialize_to_vec()
        .map_err(|_| SimulateBlockTemplateError::BlockSerializationError)?;

    let gbt = GetBlockTemplateHandler::new(
        &network,
        mining_config,
        block_verifier_router,
        sync_status.clone(),
        None,
    );

    let validation_result = validate_block_proposal(
        gbt.block_verifier_router(),
        block_bytes,
        network.clone(),
        latest_chain_tip.clone(),
        sync_status.clone(),
    )
    .await
    .map_err(|_| SimulateBlockTemplateError::InvalidBlockProposalResponse)?;

    match validation_result {
        Response::ProposalMode(response) => {
            if !response.is_valid() {
                tracing::warn!("block template is invalid for current mempool");
                Ok(false)
            } else {
                tracing::info!("block template is valid for current mempool");
                Ok(true)
            }
        }
        _ => Err(SimulateBlockTemplateError::UnexpectedValidationResponse),
    }
}
