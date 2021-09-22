use std::{collections::HashSet, net::SocketAddr, str::FromStr, sync::Arc};

use super::mempool::{unmined_transactions_in_blocks, Mempool};
use crate::components::sync::SyncStatus;

use tokio::sync::oneshot;
use tower::{builder::ServiceBuilder, util::BoxService, ServiceExt};

use tracing::Span;
use zebra_chain::{
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::Config as ConsensusConfig;
use zebra_network::{AddressBook, Request, Response};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::MockService;

#[tokio::test]
async fn mempool_requests_for_transactions() {
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let peer_set = MockService::build().for_unit_tests();
    let address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Span::none());
    let address_book = Arc::new(std::sync::Mutex::new(address_book));
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (state, _latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let state_service = ServiceBuilder::new().buffer(1).service(state);

    let (block_verifier, transaction_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    let peer_set_service = ServiceBuilder::new()
        .buffer(1)
        .service(BoxService::new(peer_set));

    let mut mempool_service = Mempool::new(
        network,
        peer_set_service.clone(),
        state_service.clone(),
        transaction_verifier,
        sync_status,
        chain_tip_change,
    );

    // Enable the mempool
    let _ = mempool_service.enable(&mut recent_syncs).await;

    let added_transactions = add_some_stuff_to_mempool(&mut mempool_service, network);
    let added_transaction_ids: Vec<UnminedTxId> = added_transactions.iter().map(|t| t.id).collect();

    let mempool_service = BoxService::new(mempool_service);
    let mempool = ServiceBuilder::new().buffer(1).service(mempool_service);

    let (setup_tx, setup_rx) = oneshot::channel();

    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .buffer(1)
        .service(super::Inbound::new(
            setup_rx,
            state_service,
            block_verifier.clone(),
        ));

    let r = setup_tx.send((peer_set_service, address_book, mempool.clone()));
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok());

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
