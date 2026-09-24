//! Tests for ZIP-317 transaction selection for block template production

#![allow(clippy::unwrap_in_result)]

use zcash_keys::address::Address;
use zcash_transparent::address::TransparentAddress;

use zebra_chain::{
    amount::Amount,
    block::{Header, Height, MAX_BLOCK_BYTES},
    parameters::Network,
    transaction,
    transparent::OutPoint,
};
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::{get_block_template::MinerParams, transaction::TransactionTemplate};

use super::{max_transaction_count_size, select_mempool_transactions};

#[test]
fn excludes_tx_with_unselected_dependencies() {
    let network = Network::Mainnet;
    let mut mempool_tx_deps = TransactionDependencies::default();

    let unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next()
        .expect("should not be empty");

    mempool_tx_deps.add(
        unmined_tx.transaction.id.mined_id(),
        vec![OutPoint::from_usize(transaction::Hash([0; 32]), 0)],
    );

    assert_eq!(
        select_mempool_transactions(
            &network,
            Height(1_000_000),
            zebra_chain::parameters::subsidy::scheduled_block_subsidy(Height(1_000_000), &network)
                .unwrap(),
            &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
            vec![unmined_tx],
            mempool_tx_deps,
            None,
        ),
        vec![],
        "should not select any transactions when dependencies are unavailable"
    );
}

#[test]
fn includes_tx_with_selected_dependencies() {
    let network = Network::Mainnet;
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

    let selected_txs = select_mempool_transactions(
        &network,
        Height(1_000_000),
        zebra_chain::parameters::subsidy::scheduled_block_subsidy(Height(1_000_000), &network)
            .unwrap(),
        &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
        unmined_txs.clone(),
        mempool_tx_deps.clone(),
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

/// Checks that transaction selection reserves space for the block header and the transaction
/// count, which [`MAX_BLOCK_BYTES`] covers: a transaction exactly filling the remaining safe
/// budget is selected, and a transaction one byte larger is not (GHSA-95m2-vx53-v2jw).
#[test]
fn reserves_space_for_block_header_and_transaction_count() {
    let network = Network::Mainnet;
    let height = Height(1_000_000);
    let miner_params =
        MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));

    let coinbase_tx_size = TransactionTemplate::new_coinbase(
        &network,
        height,
        &miner_params,
        zebra_chain::parameters::subsidy::scheduled_block_subsidy(height, &network).unwrap(),
        Amount::zero(),
    )
    .expect("valid coinbase transaction template")
    .data
    .as_ref()
    .len();

    let safe_budget = usize::try_from(MAX_BLOCK_BYTES).expect("fits in memory")
        - Header::serialized_size(&network)
        - max_transaction_count_size()
        - coinbase_tx_size;

    let mut unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next()
        .expect("should not be empty");

    unmined_tx.transaction.size = safe_budget;

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            zebra_chain::parameters::subsidy::scheduled_block_subsidy(height, &network).unwrap(),
            &miner_params,
            vec![unmined_tx.clone()],
            TransactionDependencies::default(),
            None,
        )
        .len(),
        1,
        "should select a transaction exactly filling the safe block budget"
    );

    unmined_tx.transaction.size = safe_budget + 1;

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            zebra_chain::parameters::subsidy::scheduled_block_subsidy(height, &network).unwrap(),
            &miner_params,
            vec![unmined_tx],
            TransactionDependencies::default(),
            None,
        ),
        vec![],
        "should not select a transaction one byte over the safe block budget"
    );
}

/// The real selector must leave room for a Sapling coinbase in the global ZIP-218 budget.
#[test]
fn reserves_shielded_budget_for_sapling_coinbase() {
    use super::super::CoinbaseCache;
    use crate::config::mining::{default_miner_address, MinerAddressType};
    use std::sync::Arc;
    use zebra_chain::{
        parameters::{subsidy::scheduled_block_subsidy, testnet::ConfiguredActivationHeights},
        serialization::{ZcashDeserializeInto, ZcashSerialize},
        transaction::{arbitrary::v5_transactions, Transaction, VerifiedUnminedTx},
    };
    use zebra_consensus::ShieldedActionCounts;

    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(1_000),
            ..Default::default()
        }
        .into(),
    );
    let height = Height(1_000);
    let subsidy = scheduled_block_subsidy(height, &network).unwrap();
    let miner = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Sapling),
        )
        .unwrap(),
    );
    let cache = CoinbaseCache::default();
    let sizing =
        TransactionTemplate::new_coinbase(&network, height, &miner, subsidy, Amount::zero())
            .unwrap();
    let sizing_tx: Transaction = sizing.data.as_ref().zcash_deserialize_into().unwrap();
    let coinbase_counts = ShieldedActionCounts::from_transaction(&sizing_tx);
    assert!(sizing_tx.sapling_outputs().count() > 0);
    cache.store(height, subsidy, Amount::zero(), sizing);

    let orchard = v5_transactions(Network::Mainnet.block_iter())
        .find(|tx| {
            tx.orchard_actions().count() == 2
                && tx.joinsplit_count() == 0
                && tx.sapling_spends_count() == 0
                && tx.sapling_outputs().count() == 0
        })
        .expect("fixture contains an Orchard-only two-action transaction");
    // Distinct serialized transactions ensure the selector's txid map keeps all candidates.
    let candidates: Vec<_> = (0..165)
        .map(|i| {
            let mut tx = orchard.clone();
            tx.set_expiry_height(Height(2_000 + i));
            let tx = zebra_chain::transaction::UnminedTx::from(Arc::new(tx));
            let fee = tx.conventional_fee;
            VerifiedUnminedTx::new(tx, fee, 0, 0, Arc::new(vec![])).unwrap()
        })
        .collect();
    let extra_counts =
        ShieldedActionCounts::from_transaction(&candidates[0].transaction.transaction);
    let selected = select_mempool_transactions(
        &network,
        height,
        subsidy,
        &miner,
        candidates,
        TransactionDependencies::default(),
        Some(&cache),
    );
    let counts = selected.iter().fold(coinbase_counts, |counts, (_, tx)| {
        counts.saturating_add(ShieldedActionCounts::from_transaction(
            &tx.transaction.transaction,
        ))
    });
    assert!(counts.exceeded_limit().is_none());
    assert!(counts
        .saturating_add(extra_counts)
        .exceeded_limit()
        .is_some());

    let fees = selected
        .iter()
        .map(|(_, tx)| tx.miner_fee)
        .sum::<zebra_chain::amount::Result<Amount<_>>>()
        .unwrap();
    let actual =
        TransactionTemplate::new_coinbase(&network, height, &miner, subsidy, fees).unwrap();
    let actual_tx: Transaction = actual.data.as_ref().zcash_deserialize_into().unwrap();
    assert_eq!(
        ShieldedActionCounts::from_transaction(&actual_tx),
        coinbase_counts
    );
    assert_eq!(
        actual.data.as_ref().len(),
        sizing_tx.zcash_serialized_size()
    );
}
