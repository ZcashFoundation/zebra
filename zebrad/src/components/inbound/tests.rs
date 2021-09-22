use std::{collections::HashSet, net::SocketAddr, str::FromStr, sync::Arc};

use super::mempool::{unmined_transactions_in_blocks, Mempool};
use crate::components::sync::SyncStatus;

use futures::FutureExt;
use tokio::sync::oneshot;
use tower::{
    buffer::Buffer, builder::ServiceBuilder, load_shed::LoadShed, util::BoxService, ServiceExt,
};

use tracing::Span;
use zebra_chain::{
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTx, UnminedTxId},
};

use zebra_consensus::{error::TransactionError, transaction, Config as ConsensusConfig};
use zebra_network::{AddressBook, Request, Response};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

#[tokio::test]
async fn mempool_requests_for_transactions() {
    let (inbound_service, added_transactions, _) = setup(true).await;

    let added_transaction_ids: Vec<UnminedTxId> = added_transactions
        .clone()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();

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
        Ok(Response::Transactions(response)) => assert_eq!(response, added_transactions.unwrap()),
        _ => unreachable!("`TransactionsById` requests should always respond `Ok(Vec<UnminedTx>)`"),
    };
}

#[tokio::test]
async fn mempool_push_transaction() -> Result<(), crate::BoxError> {
    // get a block that has at least one non coinbase transaction
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // use the first transaction that is not coinbase
    let tx = block.transactions[1].clone();

    let (inbound_service, _, mut tx_verifier) = setup(false).await;

    // Test `Request::PushTransaction`
    let request = inbound_service
        .clone()
        .oneshot(Request::PushTransaction(tx.clone().into()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let txid = responder.request().tx_id();
        responder.respond(txid);
    });
    let (response, _) = futures::join!(request, verification);
    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`PushTransaction` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => assert_eq!(response, vec![tx.unmined_id()]),
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    Ok(())
}

#[tokio::test]
async fn mempool_advertise_transaction_ids() -> Result<(), crate::BoxError> {
    // get a block that has at least one non coinbase transaction
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // use the first transaction that is not coinbase
    let mut txs = HashSet::new();
    txs.insert(block.transactions[1].unmined_id());

    let (inbound_service, _, mut tx_verifier) = setup(false).await;

    // Test `Request::AdvertiseTransactionIds`
    let request = inbound_service
        .clone()
        .oneshot(Request::AdvertiseTransactionIds(txs.clone()));
    // Simulate a successful transaction verification
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        let txid = responder.request().tx_id();
        responder.respond(txid);
    });
    let (response, _) = futures::join!(request, verification);

    match response {
        Ok(Response::Nil) => (),
        _ => unreachable!("`AdvertiseTransactionIds` requests should always respond `Ok(Nil)`"),
    };

    // Use `Request::MempoolTransactionIds` to check the transaction was inserted to mempool
    let request = inbound_service
        .clone()
        .oneshot(Request::MempoolTransactionIds)
        .await;

    match request {
        Ok(Response::TransactionIds(response)) => {
            assert_eq!(response, vec![block.transactions[1].unmined_id()])
        }
        _ => unreachable!(
            "`MempoolTransactionIds` requests should always respond `Ok(Vec<UnminedTxId>)`"
        ),
    };

    Ok(())
}

async fn setup(
    add_transactions: bool,
) -> (
    LoadShed<tower::buffer::Buffer<super::Inbound, zebra_network::Request>>,
    Option<Vec<UnminedTx>>,
    MockService<transaction::Request, transaction::Response, PanicAssertion, TransactionError>,
) {
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Span::none());
    let address_book = Arc::new(std::sync::Mutex::new(address_book));
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (_state_service, _latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let (state, _, _) = zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(1).service(state);

    let (block_verifier, _transaction_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    let peer_set = MockService::build().for_unit_tests();
    let buffered_peer_set = Buffer::new(BoxService::new(peer_set.clone()), 10);

    let mock_tx_verifier = MockService::build().for_unit_tests();
    let buffered_tx_verifier = Buffer::new(BoxService::new(mock_tx_verifier.clone()), 10);

    let mut mempool_service = Mempool::new(
        network,
        buffered_peer_set.clone(),
        state_service.clone(),
        buffered_tx_verifier.clone(),
        sync_status,
        chain_tip_change,
    );

    let mut added_transactions = None;
    if add_transactions {
        added_transactions = Some(add_some_stuff_to_mempool(&mut mempool_service, network));
    }

    let mempool_service = BoxService::new(mempool_service);
    let mempool = ServiceBuilder::new().buffer(1).service(mempool_service);

    let (setup_tx, setup_rx) = oneshot::channel();

    let inbound_service = ServiceBuilder::new()
        .load_shed()
        .buffer(1)
        .service(super::Inbound::new(
            setup_rx,
            state_service.clone(),
            block_verifier.clone(),
        ));

    let r = setup_tx.send((buffered_peer_set, address_book, mempool));
    // We can't expect or unwrap because the returned Result does not implement Debug
    assert!(r.is_ok());

    // Push the genesis block to the state
    let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .oneshot(zebra_state::Request::CommitFinalizedBlock(
            genesis_block.clone().into(),
        ))
        .await
        .unwrap();

    (inbound_service, added_transactions, mock_tx_verifier)
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
