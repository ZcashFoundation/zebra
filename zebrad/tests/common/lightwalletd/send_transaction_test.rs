//! Test sending transactions using a lightwalletd instance connected to a zebrad instance.
//!
//! This test requires a cached chain state that is partially synchronized, i.e., it should be a
//! few blocks below the network chain tip height. We open this state during the test, but we don't
//! add any blocks to it.
//!
//! The transactions to use to send are obtained from the blocks synchronized by a temporary zebrad
//! instance that are higher than the chain tip of the cached state. This instance uses a copy of
//! the state.
//!
//! The zebrad instance connected to lightwalletd uses the cached state and does not connect to any
//! external peers, which prevents it from downloading the blocks from where the test transactions
//! were obtained. This is to ensure that zebra does not reject the transactions because they have
//! already been seen in a block.

use std::{
    cmp::min,
    path::{Path, PathBuf},
    sync::Arc,
};

use color_eyre::eyre::{eyre, Result};
use futures::TryFutureExt;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block, chain_tip::ChainTip, parameters::Network, serialization::ZcashSerialize,
    transaction::Transaction,
};
use zebra_rpc::queue::CHANNEL_AND_QUEUE_CAPACITY;
use zebra_state::HashOrHeight;
use zebrad::components::mempool::downloads::MAX_INBOUND_CONCURRENCY;

use crate::common::{
    cached_state::{load_tip_height_from_state_directory, start_state_service_with_cache_dir},
    launch::spawn_zebrad_for_rpc_without_initial_peers,
    lightwalletd::{
        wallet_grpc::{self, connect_to_lightwalletd, spawn_lightwalletd_with_rpc_server},
        zebra_skip_lightwalletd_tests,
        LightwalletdTestType::*,
    },
    sync::perform_full_sync_starting_from,
};

/// The test entry point.
pub async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Skip the test unless the user specifically asked for it
    if zebra_skip_lightwalletd_tests() {
        return Ok(());
    }

    // We want a zebra state dir and a lightwalletd data dir in place,
    // so `UpdateCachedState` can be used as our test type
    let test_type = UpdateCachedState;

    let zebrad_state_path = test_type.zebrad_state_path("send_transaction_tests".to_string());
    let zebrad_state_path = match zebrad_state_path {
        Some(zebrad_state_path) => zebrad_state_path,
        None => return Ok(()),
    };

    let lightwalletd_state_path =
        test_type.lightwalletd_state_path("send_transaction_tests".to_string());
    if lightwalletd_state_path.is_none() {
        return Ok(());
    }

    let network = Network::Mainnet;

    tracing::info!(
        ?network,
        ?test_type,
        ?zebrad_state_path,
        ?lightwalletd_state_path,
        "running gRPC send transaction test using lightwalletd & zebrad",
    );

    let mut transactions =
        load_transactions_from_a_future_block(network, zebrad_state_path.clone()).await?;

    tracing::info!(
        transaction_count = ?transactions.len(),
        partial_sync_path = ?zebrad_state_path,
        "got transactions to send",
    );

    let (_zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc_without_initial_peers(Network::Mainnet, zebrad_state_path, test_type)?;

    tracing::info!(
        ?zebra_rpc_address,
        "spawned disconnected zebrad with shorter chain",
    );

    let (_lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_with_rpc_server(
        zebra_rpc_address,
        lightwalletd_state_path,
        test_type,
        true,
    )?;

    tracing::info!(
        ?lightwalletd_rpc_port,
        "spawned lightwalletd connected to zebrad",
    );

    let mut rpc_client = connect_to_lightwalletd(lightwalletd_rpc_port).await?;

    // To avoid filling the mempool queue, limit the transactions to be sent to the RPC and mempool queue limits
    transactions.truncate(min(CHANNEL_AND_QUEUE_CAPACITY, MAX_INBOUND_CONCURRENCY) - 1);

    tracing::info!(
        transaction_count = ?transactions.len(),
        "connected gRPC client to lightwalletd, sending transactions...",
    );

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
/// We copy the cached state to avoid modifying `zebrad_state_path`.
/// This copy is used to launch a `zebrad` instance connected to the network,
/// which finishes synchronizing the chain.
/// Then we load transactions from this updated state.
///
/// Returns a list of valid transactions that are not in any of the blocks present in the
/// original `zebrad_state_path`.
async fn load_transactions_from_a_future_block(
    network: Network,
    zebrad_state_path: PathBuf,
) -> Result<Vec<Arc<Transaction>>> {
    let partial_sync_height =
        load_tip_height_from_state_directory(network, zebrad_state_path.as_ref()).await?;

    tracing::info!(
        ?partial_sync_height,
        partial_sync_path = ?zebrad_state_path,
        "performing full sync...",
    );

    let full_sync_path =
        perform_full_sync_starting_from(network, zebrad_state_path.as_ref()).await?;

    tracing::info!(?full_sync_path, "loading transactions...");

    let transactions =
        load_transactions_from_block_after(partial_sync_height, network, full_sync_path.as_ref())
            .await?;

    Ok(transactions)
}

/// Loads transactions from a block that's after the specified `height`.
///
/// Starts at the block after the block at the specified `height`, and stops when it finds a block
/// from where it can load at least one non-coinbase transaction.
///
/// # Panics
///
/// If the specified `zebrad_state_path` contains a chain state that's not synchronized to a tip that's
/// after `height`.
async fn load_transactions_from_block_after(
    height: block::Height,
    network: Network,
    zebrad_state_path: &Path,
) -> Result<Vec<Arc<Transaction>>> {
    let (_read_write_state_service, mut state, latest_chain_tip, _chain_tip_change) =
        start_state_service_with_cache_dir(network, zebrad_state_path).await?;

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
