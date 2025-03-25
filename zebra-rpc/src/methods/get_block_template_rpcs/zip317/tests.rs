//! Tests for ZIP-317 transaction selection for block template production
use zebra_chain::{
    block::{subsidy::general, Height},
    parameters::Network,
    transaction,
    transparent::{self, OutPoint},
};
use zebra_node_services::mempool::TransactionDependencies;

use super::select_mempool_transactions;

#[cfg(zcash_unstable = "zip234")]
use zebra_chain::amount::MAX_MONEY;

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

    #[cfg(zcash_unstable = "zip234")]
    let expected_block_subsidy = general::block_subsidy(
        next_block_height,
        &network,
        MAX_MONEY.try_into().expect("MAX_MONEY is a valid amount"),
    )
    .expect("failed to get block subsidy");
    #[cfg(not(zcash_unstable = "zip234"))]
    let expected_block_subsidy = general::block_subsidy_pre_nsm(next_block_height, &network)
        .expect("failed to get block subsidy");

    assert_eq!(
        select_mempool_transactions(
            &network,
            next_block_height,
            &miner_address,
            vec![unmined_tx],
            mempool_tx_deps,
            like_zcashd,
            extra_coinbase_data,
            expected_block_subsidy,
            None,
        ),
        vec![],
        "should not select any transactions when dependencies are unavailable"
    );
}

#[test]
fn includes_tx_with_selected_dependencies() {
    let network = Network::Mainnet;
    let next_block_height = Height(1_000_000);
    let miner_address = transparent::Address::from_pub_key_hash(network.kind(), [0; 20]);
    let unmined_txs: Vec<_> = network.unmined_transactions_in_blocks(..).take(3).collect();

    let dependent_tx1 = unmined_txs.first().expect("should have 3 txns");
    let dependent_tx2 = unmined_txs.get(1).expect("should have 3 txns");
    let independent_tx_id = unmined_txs
        .get(2)
        .expect("should have 3 txns")
        .transaction
        .id
        .mined_id();

    let mut mempool_tx_deps = TransactionDependencies::default();
    mempool_tx_deps.add(
        dependent_tx1.transaction.id.mined_id(),
        vec![OutPoint::from_usize(independent_tx_id, 0)],
    );
    mempool_tx_deps.add(
        dependent_tx2.transaction.id.mined_id(),
        vec![
            OutPoint::from_usize(independent_tx_id, 0),
            OutPoint::from_usize(transaction::Hash([0; 32]), 0),
        ],
    );

    let like_zcashd = true;
    let extra_coinbase_data = Vec::new();

    #[cfg(zcash_unstable = "zip234")]
    let expected_block_subsidy = general::block_subsidy(
        next_block_height,
        &network,
        MAX_MONEY.try_into().expect("MAX_MONEY is a valid amount"),
    )
    .expect("failed to get block subsidy");
    #[cfg(not(zcash_unstable = "zip234"))]
    let expected_block_subsidy = general::block_subsidy_pre_nsm(next_block_height, &network)
        .expect("failed to get block subsidy");

    let selected_txs = select_mempool_transactions(
        &network,
        next_block_height,
        &miner_address,
        unmined_txs.clone(),
        mempool_tx_deps.clone(),
        like_zcashd,
        extra_coinbase_data,
        expected_block_subsidy,
        None,
    );

    assert_eq!(
        selected_txs.len(),
        2,
        "should select the independent transaction and 1 of the dependent txs, selected: {selected_txs:?}"
    );

    let selected_tx_by_id = |id| {
        selected_txs
            .iter()
            .find(|(_, tx)| tx.transaction.id.mined_id() == id)
    };

    let (dependency_depth, _) =
        selected_tx_by_id(independent_tx_id).expect("should select the independent tx");

    assert_eq!(
        *dependency_depth, 0,
        "should return a dependency depth of 0 for the independent tx"
    );

    let (dependency_depth, _) = selected_tx_by_id(dependent_tx1.transaction.id.mined_id())
        .expect("should select dependent_tx1");

    assert_eq!(
        *dependency_depth, 1,
        "should return a dependency depth of 1 for the dependent tx"
    );
}
