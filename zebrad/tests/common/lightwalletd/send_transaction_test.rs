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
    block,
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashSerialize,
    transaction::{self, Transaction},
};
use zebra_rpc::queue::CHANNEL_AND_QUEUE_CAPACITY;
use zebra_state::HashOrHeight;
use zebrad::components::mempool::downloads::MAX_INBOUND_CONCURRENCY;

use crate::common::{
    cached_state::{load_tip_height_from_state_directory, start_state_service_with_cache_dir},
    launch::spawn_zebrad_for_rpc_without_initial_peers,
    lightwalletd::{
        wallet_grpc::{
            self, connect_to_lightwalletd, spawn_lightwalletd_with_rpc_server, Empty, Exclude,
        },
        zebra_skip_lightwalletd_tests,
        LightwalletdTestType::*,
    },
    sync::copy_state_and_perform_full_sync,
};

/// The maximum number of transactions we want to send in the test.
/// This avoids filling the mempool queue and generating errors.
///
/// TODO: replace with a const when `min()` stabilises as a const function:
///       https://github.com/rust-lang/rust/issues/92391
fn max_sent_transactions() -> usize {
    min(CHANNEL_AND_QUEUE_CAPACITY, MAX_INBOUND_CONCURRENCY) - 1
}

/// The test entry point.
//
// TODO:
// - check output of zebrad and lightwalletd in different threads,
//   to avoid test hangs due to full output pipes
//   (see lightwalletd_integration_test for an example)
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
        load_transactions_from_future_blocks(network, zebrad_state_path.clone()).await?;

    tracing::info!(
        transaction_count = ?transactions.len(),
        partial_sync_path = ?zebrad_state_path,
        "got transactions to send",
    );

    // TODO: change debug_skip_parameter_preload to true if we do the mempool test in the wallet gRPC test
    let (mut zebrad, zebra_rpc_address) = spawn_zebrad_for_rpc_without_initial_peers(
        Network::Mainnet,
        zebrad_state_path,
        test_type,
        false,
    )?;

    tracing::info!(
        ?zebra_rpc_address,
        "spawned disconnected zebrad with shorter chain, waiting for mempool activation...",
    );

    let (_lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_with_rpc_server(
        zebra_rpc_address,
        lightwalletd_state_path,
        test_type,
        true,
    )?;

    tracing::info!(
        ?lightwalletd_rpc_port,
        "spawned lightwalletd connected to zebrad, waiting for zebrad mempool activation...",
    );

    zebrad.expect_stdout_line_matches("activating mempool")?;

    // TODO: check that lightwalletd is at the tip using gRPC (#4894)
    //
    // If this takes a long time, we might need to check zebrad logs for failures in a separate thread.

    tracing::info!(
        ?lightwalletd_rpc_port,
        "connecting gRPC client to lightwalletd...",
    );

    let mut rpc_client = connect_to_lightwalletd(lightwalletd_rpc_port).await?;

    // To avoid filling the mempool queue, limit the transactions to be sent to the RPC and mempool queue limits
    transactions.truncate(max_sent_transactions());

    let transaction_hashes: Vec<transaction::Hash> =
        transactions.iter().map(|tx| tx.hash()).collect();

    tracing::info!(
        transaction_count = ?transactions.len(),
        ?transaction_hashes,
        "connected gRPC client to lightwalletd, sending transactions...",
    );

    for transaction in transactions {
        let transaction_hash = transaction.hash();

        let expected_response = wallet_grpc::SendResponse {
            error_code: 0,
            error_message: format!("\"{}\"", transaction_hash),
        };

        tracing::info!(?transaction_hash, "sending transaction...");

        let request = prepare_send_transaction_request(transaction);

        let response = rpc_client.send_transaction(request).await?.into_inner();

        assert_eq!(response, expected_response);
    }

    // The timing of verification logs are unrealiable, so we've disabled this check for now.
    //
    // TODO: when lightwalletd starts returning transactions again:
    //       re-enable this check, find a better way to check, or delete this commented-out check
    //
    //tracing::info!("waiting for mempool to verify some transactions...");
    //zebrad.expect_stdout_line_matches("sending mempool transaction broadcast")?;

    tracing::info!("calling GetMempoolTx gRPC to fetch transactions...");
    let mut transactions_stream = rpc_client
        .get_mempool_tx(Exclude { txid: vec![] })
        .await?
        .into_inner();

    // We'd like to check that lightwalletd queries the mempool, but it looks like it doesn't do it after each GetMempoolTx request.
    //zebrad.expect_stdout_line_matches("answered mempool request req=TransactionIds")?;

    // GetMempoolTx: make sure at least one of the transactions were inserted into the mempool.
    let mut counter = 0;
    while let Some(tx) = transactions_stream.message().await? {
        let hash: [u8; 32] = tx.hash.clone().try_into().expect("hash is correct length");
        let hash = transaction::Hash::from_bytes_in_display_order(&hash);

        assert!(
            transaction_hashes.contains(&hash),
            "unexpected transaction {hash:?}\n\
             in isolated mempool: {tx:?}",
        );

        counter += 1;
    }

    // This RPC has temporarily been disabled in `lightwalletd`:
    // https://github.com/adityapk00/lightwalletd/blob/b563f765f620e38f482954cd8ff3cc6d17cf2fa7/frontend/service.go#L529-L531
    //
    // TODO: re-enable it when lightwalletd starts returning transactions again.
    //assert!(counter >= 1, "all transactions from future blocks failed to send to an isolated mempool");
    assert_eq!(
        counter, 0,
        "developers: update this test for lightwalletd sending transactions"
    );

    // GetMempoolTx: make sure at least one of the transactions were inserted into the mempool.
    tracing::info!("calling GetMempoolStream gRPC to fetch transactions...");
    let mut transaction_stream = rpc_client.get_mempool_stream(Empty {}).await?.into_inner();

    let mut counter = 0;
    while let Some(_tx) = transaction_stream.message().await? {
        // TODO: check tx.data or tx.height here?

        counter += 1;
    }

    // This RPC has temporarily been disabled in `lightwalletd`:
    // https://github.com/adityapk00/lightwalletd/blob/b563f765f620e38f482954cd8ff3cc6d17cf2fa7/frontend/service.go#L515-L517
    //
    // TODO: re-enable it when lightwalletd starts streaming transactions again.
    //assert!(counter >= 1, "all transactions from future blocks failed to send to an isolated mempool");
    assert_eq!(
        counter, 0,
        "developers: update this test for lightwalletd sending transactions"
    );

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
#[tracing::instrument]
async fn load_transactions_from_future_blocks(
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
        copy_state_and_perform_full_sync(network, zebrad_state_path.as_ref()).await?;

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
#[tracing::instrument]
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
        "Chain not synchronized to a block after the specified height",
    );

    let mut target_height = height.0;
    let mut transactions = Vec::new();

    while transactions.len() < max_sent_transactions() {
        let new_transactions =
            load_transactions_from_block(block::Height(target_height), &mut state).await?;

        if let Some(mut new_transactions) = new_transactions {
            new_transactions.retain(|transaction| !transaction.is_coinbase());
            transactions.append(&mut new_transactions);
        } else {
            tracing::info!(
                "Reached the end of the finalized chain\n\
                 collected {} transactions from {} blocks before {target_height:?}",
                transactions.len(),
                target_height - height.0 - 1,
            );
            break;
        }

        target_height += 1;
    }

    tracing::info!(
        "Collected {} transactions from {} blocks before {target_height:?}",
        transactions.len(),
        target_height - height.0 - 1,
    );

    Ok(transactions)
}

/// Performs a request to the provided read-only `state` service to fetch all transactions from a
/// block at the specified `height`.
#[tracing::instrument(skip(state))]
async fn load_transactions_from_block<ReadStateService>(
    height: block::Height,
    state: &mut ReadStateService,
) -> Result<Option<Vec<Arc<Transaction>>>>
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
            tracing::info!(
                "Reached the end of the finalized chain, state is missing block at {height:?}",
            );
            return Ok(None);
        }
        _ => unreachable!("Incorrect response from state service: {response:?}"),
    };

    Ok(Some(block.transactions.to_vec()))
}

/// Prepare a request to send to lightwalletd that contains a transaction to be sent.
fn prepare_send_transaction_request(transaction: Arc<Transaction>) -> wallet_grpc::RawTransaction {
    let transaction_bytes = transaction.zcash_serialize_to_vec().unwrap();

    wallet_grpc::RawTransaction {
        data: transaction_bytes,
        height: -1,
    }
}
