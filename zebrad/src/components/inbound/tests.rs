use tower::ServiceExt;

use super::mempool::{unmined_transactions_in_blocks, Mempool};

use tokio::sync::oneshot;
use tower::builder::ServiceBuilder;

use zebra_chain::{
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::Config;
use zebra_network::{Request, Response};

#[tokio::test]
async fn mempool_requests_for_transaction_ids() {
    let network = Network::Mainnet;
    let config = Config::default();

    let state_service = zebra_state::init_test(network);
    let mut mempool_service = Mempool::new(network);

    let added_transaction_ids: Vec<UnminedTxId> =
        add_some_stuff_to_mempool(&mut mempool_service, network)
            .iter()
            .map(|t| t.id)
            .collect();

    let (chain_verifier, _transaction_verifier) =
        zebra_consensus::chain::init(config.clone(), network, zebra_state::init_test(network))
            .await;
    let (_setup_tx, setup_rx) = oneshot::channel();

    let inbound_service =
        ServiceBuilder::new()
            .load_shed()
            .buffer(20)
            .service(super::Inbound::new(
                setup_rx,
                state_service.clone(),
                chain_verifier.clone(),
                mempool_service,
            ));

    let request = inbound_service
        .oneshot(Request::MempoolTransactionIds)
        .await;
    match request {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, added_transaction_ids),
        _ => panic!("in this test we are pretty sure there is always a correct response"),
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
