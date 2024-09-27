//! Tests for ZIP-317 transaction selection for block template production

use zebra_chain::{
    block::Height,
    parameters::Network,
    transaction,
    transparent::{self, OutPoint},
};
use zebra_node_services::mempool::TransactionDependencies;

use super::select_mempool_transactions;

#[test]
fn excludes_tx_with_unselected_dependencies() {
    let network = Network::Mainnet;
    let next_block_height = Height(1_000_000);
    let miner_address = transparent::Address::from_pub_key_hash(network.kind(), [0; 20]);
    let unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next()
        .expect("should not be empty");

    let mut mempool_tx_deps = TransactionDependencies::default();
    mempool_tx_deps.add(
        unmined_tx.transaction.id.mined_id(),
        vec![OutPoint::from_usize(transaction::Hash([0; 32]), 0)],
    );

    let like_zcashd = true;
    let extra_coinbase_data = Vec::new();

    assert_eq!(
        select_mempool_transactions(
            &network,
            next_block_height,
            &miner_address,
            vec![unmined_tx],
            mempool_tx_deps,
            like_zcashd,
            extra_coinbase_data,
        ),
        vec![],
        "should not select any transactions when dependencies are unavailable"
    );
}
