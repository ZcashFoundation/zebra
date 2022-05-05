//! Test sending transactions using a lightwalletd instance connected to a zebrad instance.
//!
//! This test requires a cached chain state that is partially synchronized, i.e., it should be a
//! few blocks below the network chain tip height.
//!
//! The transactions to use to send are obtained from the blocks synchronized by a temporary zebrad
//! instance that are higher than the chain tip of the cached state.
//!
//! The zebrad instance connected to lightwalletd uses the cached state and does not connect to any
//! external peers, which prevents it from downloading the blocks from where the test transactions
//! were obtained. This is to ensure that zebra does not reject the transactions because they have
//! already been seen in a block.

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use color_eyre::eyre::{eyre, Result};
use futures::TryFutureExt;
use tempfile::TempDir;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block, chain_tip::ChainTip, parameters::Network, serialization::ZcashSerialize,
    transaction::Transaction,
};
use zebra_state::HashOrHeight;

use crate::common::{
    cached_state::{
        copy_state_directory, load_tip_height_from_state_directory,
        start_state_service_with_cache_dir, ZEBRA_CACHED_STATE_DIR_VAR,
    },
    launch::spawn_zebrad_for_rpc_without_initial_peers,
    lightwalletd::{
        wallet_grpc::{self, connect_to_lightwalletd, spawn_lightwalletd_with_rpc_server},
        zebra_skip_lightwalletd_tests, LIGHTWALLETD_TEST_TIMEOUT,
    },
    sync::perform_full_sync_starting_from,
};

/// The test entry point.
pub async fn run() -> Result<()> {
    zebra_test::init();

    // Skip the test unless the user specifically asked for it
    if zebra_skip_lightwalletd_tests() {
        return Ok(());
    }

    let cached_state_path = match env::var_os(ZEBRA_CACHED_STATE_DIR_VAR) {
        Some(argument) => PathBuf::from(argument),
        None => {
            tracing::info!(
                "skipped send transactions using lightwalletd test, \
                 set the {ZEBRA_CACHED_STATE_DIR_VAR:?} environment variable to run the test",
            );
            return Ok(());
        }
    };

    let network = Network::Mainnet;

    let (transactions, partial_sync_path) =
        load_transactions_from_a_future_block(network, cached_state_path).await?;

    let (_zebrad, zebra_rpc_address) = spawn_zebrad_for_rpc_without_initial_peers(
        Network::Mainnet,
        partial_sync_path,
        LIGHTWALLETD_TEST_TIMEOUT,
    )?;

    let (_lightwalletd, lightwalletd_rpc_port) =
        spawn_lightwalletd_with_rpc_server(zebra_rpc_address, true)?;

    let mut rpc_client = connect_to_lightwalletd(lightwalletd_rpc_port).await?;

    for transaction in transactions {
        let expected_response = wallet_grpc::SendResponse {
            error_code: 0,
            error_message: format!("\"{}\"", transaction.hash()),
        };

        let request = prepare_send_transaction_request(transaction);

        let response = rpc_client.send_transaction(request).await?.into_inner();

        assert_eq!(response, expected_response);
    }

    Ok(())
}

/// Loads transactions from a block that's after the chain tip of the cached state.
///
/// This copies the cached state into a temporary directory when it is needed to avoid overwriting
/// anything. Two copies are made of the cached state.
///
/// The first copy is used by a zebrad instance connected to the network that finishes
/// synchronizing the chain. The transactions are loaded from this updated state.
///
/// The second copy of the state is returned together with the transactions. This means that the
/// returned tuple contains the temporary directory with the partially synchronized chain, and a
/// list of valid transactions that are not in any of the blocks present in that partially
/// synchronized chain.
async fn load_transactions_from_a_future_block(
    network: Network,
    cached_state_path: PathBuf,
) -> Result<(Vec<Arc<Transaction>>, TempDir)> {
    let (partial_sync_path, partial_sync_height) =
        prepare_partial_sync(network, cached_state_path).await?;

    let full_sync_path =
        perform_full_sync_starting_from(network, partial_sync_path.as_ref()).await?;

    let transactions =
        load_transactions_from_block_after(partial_sync_height, network, full_sync_path.as_ref())
            .await?;

    Ok((transactions, partial_sync_path))
}

/// Prepares the temporary directory of the partially synchronized chain.
///
/// Returns a temporary directory that can be used by a Zebra instance, as well as the chain tip
/// height of the partially synchronized chain.
async fn prepare_partial_sync(
    network: Network,
    cached_zebra_state: PathBuf,
) -> Result<(TempDir, block::Height)> {
    let partial_sync_path = copy_state_directory(cached_zebra_state).await?;
    let partial_sync_state_dir = partial_sync_path.as_ref().join("state");
    let tip_height = load_tip_height_from_state_directory(network, &partial_sync_state_dir).await?;

    Ok((partial_sync_path, tip_height))
}

/// Loads transactions from a block that's after the specified `height`.
///
/// Starts at the block after the block at the specified `height`, and stops when it finds a block
/// from where it can load at least one non-coinbase transaction.
///
/// # Panics
///
/// If the specified `state_path` contains a chain state that's not synchronized to a tip that's
/// after `height`.
async fn load_transactions_from_block_after(
    height: block::Height,
    network: Network,
    state_path: &Path,
) -> Result<Vec<Arc<Transaction>>> {
    let (_read_write_state_service, mut state, latest_chain_tip, _chain_tip_change) =
        start_state_service_with_cache_dir(network, state_path.join("state")).await?;

    let tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    assert!(
        tip_height > height,
        "Chain not synchronized to a block after the specified height"
    );

    let mut target_height = height.0;
    let mut transactions = Vec::new();

    while transactions.is_empty() {
        transactions =
            load_transactions_from_block(block::Height(target_height), &mut state).await?;

        transactions.retain(|transaction| !transaction.is_coinbase());

        target_height += 1;
    }

    Ok(transactions)
}

/// Performs a request to the provided read-only `state` service to fetch all transactions from a
/// block at the specified `height`.
async fn load_transactions_from_block<ReadStateService>(
    height: block::Height,
    state: &mut ReadStateService,
) -> Result<Vec<Arc<Transaction>>>
where
    ReadStateService: Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
{
    let request = zebra_state::ReadRequest::Block(HashOrHeight::Height(height));

    let response = state
        .ready()
        .and_then(|ready_service| ready_service.call(request))
        .map_err(|error| eyre!(error))
        .await?;

    let block = match response {
        zebra_state::ReadResponse::Block(Some(block)) => block,
        zebra_state::ReadResponse::Block(None) => {
            panic!("Missing block at {height:?} from state")
        }
        _ => unreachable!("Incorrect response from state service: {response:?}"),
    };

    Ok(block.transactions.to_vec())
}

/// Prepare a request to send to lightwalletd that contains a transaction to be sent.
fn prepare_send_transaction_request(transaction: Arc<Transaction>) -> wallet_grpc::RawTransaction {
    let transaction_bytes = transaction.zcash_serialize_to_vec().unwrap();

    wallet_grpc::RawTransaction {
        data: transaction_bytes,
        height: -1,
    }
}
