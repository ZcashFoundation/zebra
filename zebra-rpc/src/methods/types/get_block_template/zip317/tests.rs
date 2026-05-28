//! Tests for ZIP-317 transaction selection for block template production

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use zcash_keys::address::Address;
use zcash_transparent::address::TransparentAddress;

use zebra_chain::{
    block::Height,
    parameters::{
        Network, GLOBAL_SHIELDED_BUDGET, ORCHARD_BLOCK_ACTION_LIMIT, SAPLING_BLOCK_IO_LIMIT,
        SPROUT_BLOCK_JOINSPLIT_LIMIT,
    },
    transaction::{
        self,
        arbitrary::{fake_v5_with_orchard_actions, fake_v5_with_sapling_outputs},
        Transaction, UnminedTx, VerifiedUnminedTx,
    },
    transparent::OutPoint,
};
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::get_block_template::MinerParams;

use super::{select_mempool_transactions, BlockTemplateLimits};

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
            &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
            vec![unmined_tx],
            mempool_tx_deps,
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            None,
        ),
        vec![],
        "should not select any transactions when dependencies are unavailable"
    );
}

#[test]
fn template_limits_selection_to_global_shielded_budget_after_nu7() {
    let mut limits = nu7_block_template_limits();
    let orchard_tx = verified_unmined_tx(fake_v5_with_orchard_actions(
        usize::try_from(ORCHARD_BLOCK_ACTION_LIMIT).expect("limit fits in usize"),
    ));
    let sapling_tx = verified_unmined_tx(fake_v5_with_sapling_outputs(1));

    assert!(limits.try_add(&orchard_tx));
    assert!(!limits.try_add(&sapling_tx));
}

fn nu7_block_template_limits() -> BlockTemplateLimits {
    BlockTemplateLimits {
        remaining_bytes: usize::MAX,
        remaining_sigops: u32::MAX,
        remaining_unpaid_actions: u32::MAX,
        remaining_orchard_actions: ORCHARD_BLOCK_ACTION_LIMIT,
        remaining_sapling_ios: SAPLING_BLOCK_IO_LIMIT,
        remaining_sprout_joinsplits: SPROUT_BLOCK_JOINSPLIT_LIMIT,
        remaining_shielded_cost: GLOBAL_SHIELDED_BUDGET,
    }
}

fn verified_unmined_tx(transaction: Arc<Transaction>) -> VerifiedUnminedTx {
    let unmined_tx = UnminedTx::from(transaction);
    let miner_fee = unmined_tx.conventional_fee;

    VerifiedUnminedTx::new(unmined_tx, miner_fee, 0, 0, Arc::new(Vec::new()))
        .expect("fake transaction pays the conventional fee")
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
        &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
        unmined_txs.clone(),
        mempool_tx_deps.clone(),
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
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
