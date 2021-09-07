use std::collections::HashSet;

use super::mempool::{unmined_transactions_in_blocks, Mempool};

use tokio::sync::oneshot;
use tower::{builder::ServiceBuilder, util::BoxService, ServiceExt};

use zebra_chain::{
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::Config as ConsensusConfig;
use zebra_network::{Request, Response};
use zebra_state::Config as StateConfig;

#[tokio::test]
async fn mempool_requests_for_transactions() {
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();

    let (state, _, _) = zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(1).service(state);
    let mut mempool_service = Mempool::new(network);

    let added_transactions = add_some_stuff_to_mempool(&mut mempool_service, network);
    let added_transaction_ids: Vec<UnminedTxId> = added_transactions.iter().map(|t| t.id).collect();

    let mempool_service = BoxService::new(mempool_service);
    let mempool = ServiceBuilder::new().buffer(20).service(mempool_service);

    let (block_verifier, transaction_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;
    let (_setup_tx, setup_rx) = oneshot::channel();

    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .buffer(1)
        .service(super::Inbound::new(
            setup_rx,
            state_service,
            block_verifier.clone(),
            transaction_verifier.clone(),
            mempool,
        ));

    // Test `Request::MempoolTransactionIds`
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;
    match request {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, added_transaction_ids),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    // Test `Request::TransactionsById`
    let hash_set = added_transaction_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();

    let request = inbound_service
        .oneshot(Request::TransactionsById(hash_set))
        .await;

    match request {
        Ok(Response::Transactions(response)) => assert_eq!(response, added_transactions),
        _ => unreachable!("`TransactionsById` requests should always respond `Ok(Vec<UnminedTx>)`"),
    };
}

fn add_some_stuff_to_mempool(mempool_service: &mut Mempool, network: Network) -> Vec<UnminedTx> {
    // get the genesis block transactions from the Zcash blockchain.
    let genesis_transactions = unmined_transactions_in_blocks(0, network);
    // Insert the genesis block coinbase transaction into the mempool storage.
    mempool_service
        .storage()
        .insert(genesis_transactions.1[0].clone())
        .unwrap();

    genesis_transactions.1
}
