//! Borrowed UTXO accounting agrees with the independent output-map calculation.

use std::collections::HashMap;

use crate::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transparent,
    value_balance::ValueBalance,
};

#[test]
fn ordered_utxo_accounting_matches_output_maps_and_block_fees() {
    let _init_guard = zebra_test::init();
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into()
        .expect("the historical block vector deserializes");
    let ordered: HashMap<_, _> = block
        .transactions
        .iter()
        .flat_map(|tx| tx.spent_outpoints())
        .map(|outpoint| {
            let output = transparent::Output {
                value: Amount::<NonNegative>::zero(),
                lock_script: transparent::Script::new(&[0x51; 10_000]),
            };
            (
                outpoint,
                transparent::OrderedUtxo::new(output, Height(1), 0),
            )
        })
        .collect();
    let owned = transparent::utxos_from_ordered_utxos(ordered.clone());
    let outputs = ordered
        .iter()
        .map(|(outpoint, utxo)| (*outpoint, utxo.utxo.output.clone()))
        .collect();
    for tx in &block.transactions {
        let expected = tx.transparent_value_balance_from_outputs(&outputs).unwrap()
            + tx.sprout_value_balance().unwrap()
            + tx.sapling_value_balance()
            + tx.orchard_value_balance()
            + tx.ironwood_value_balance();
        assert_eq!(
            tx.spent_outpoints().collect::<Vec<_>>(),
            tx.inputs()
                .iter()
                .filter_map(transparent::Input::outpoint)
                .collect::<Vec<_>>(),
        );
        assert_eq!(tx.value_balance(&owned), expected);
        assert_eq!(tx.value_balance_from_ordered_utxos(&ordered), expected);
        for input in tx.inputs() {
            assert_eq!(
                input.value(&owned),
                input.value_from_ordered_utxos(&ordered)
            );
        }
    }
    assert_eq!(
        block.chain_value_pool_change_and_fees_from_ordered_utxos(
            &ordered,
            DeferredPoolBalanceChange::zero(),
            &Network::Mainnet,
            ValueBalance::zero(),
        ),
        block.chain_value_pool_change_and_fees(
            &owned,
            DeferredPoolBalanceChange::zero(),
            &Network::Mainnet,
            ValueBalance::zero(),
        ),
    );
}
