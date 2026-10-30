use super::*;
use crate::service::non_finalized_state::Chain;
use zebra_chain::{
    amount::MAX_MONEY,
    transparent::{Output, Script},
};

/// Replay the pre-optimization non-tree indexes, including the allocating transparent accessors.
/// The caller also compares complete chain snapshots after tip rollback and reapplication.
pub(in crate::service::non_finalized_state) fn assert_legacy_indexes(chain: &Chain) {
    let mut created = HashMap::new();
    let mut spent = HashMap::new();
    let mut addresses: HashMap<transparent::Address, TransparentTransfers> = HashMap::new();
    let mut locations = HashMap::new();
    let mut heights = HashMap::new();
    let mut sprout = HashMap::new();
    let mut sapling = HashMap::new();
    let mut orchard = HashMap::new();
    let mut ironwood = HashMap::new();
    for block in chain.blocks.values() {
        assert!(heights.insert(block.hash, block.height).is_none());
        let transactions = block
            .block
            .transactions
            .iter()
            .zip(block.transaction_hashes.iter())
            .enumerate();
        for (index, (transaction, hash)) in transactions.clone().skip(1).chain(transactions.take(1))
        {
            assert!(locations
                .insert(*hash, TransactionLocation::from_usize(block.height, index))
                .is_none());
            #[cfg(feature = "indexer")]
            let spending_id = *hash;
            #[cfg(not(feature = "indexer"))]
            let spending_id = ();
            sprout.extend(transaction.sprout_nullifiers().map(|nf| (nf, spending_id)));
            sapling.extend(transaction.sapling_nullifiers().map(|nf| (nf, spending_id)));
            orchard.extend(transaction.orchard_nullifiers().map(|nf| (nf, spending_id)));
            ironwood.extend(
                transaction
                    .ironwood_nullifiers()
                    .map(|nf| (nf, spending_id)),
            );
            let inputs = transaction.inputs();
            let outputs = transaction.outputs();
            for input in inputs {
                let Some(outpoint) = input.outpoint() else {
                    continue;
                };
                assert!(spent.insert(outpoint, spending_id).is_none());
                let utxo = block.spent_outputs.get(&outpoint).unwrap();
                if let Some(address) = utxo.utxo.output.address(&chain.network) {
                    let transfers = addresses.entry(address).or_default();
                    transfers.balance = (transfers.balance
                        - utxo
                            .utxo
                            .output
                            .value()
                            .constrain::<NegativeAllowed>()
                            .unwrap())
                    .unwrap();
                    let location =
                        OutputLocation::from_outpoint(transaction_location(utxo), &outpoint);
                    assert!(transfers.spent_utxos.insert(location));
                    transfers.tx_ids.insert(*hash);
                }
            }
            for output_index in 0..outputs.len() {
                let outpoint = transparent::OutPoint {
                    hash: *hash,
                    index: output_index.try_into().unwrap(),
                };
                let utxo = block.new_outputs.get(&outpoint).unwrap();
                assert!(created.insert(outpoint, Arc::new(utxo.clone())).is_none());
                if let Some(address) = utxo.utxo.output.address(&chain.network) {
                    let transfers = addresses.entry(address).or_default();
                    transfers.balance = (transfers.balance
                        + utxo
                            .utxo
                            .output
                            .value()
                            .constrain::<NegativeAllowed>()
                            .unwrap())
                    .unwrap();
                    let location =
                        OutputLocation::from_outpoint(transaction_location(utxo), &outpoint);
                    // Keep this copy independent of the chain index, as in the old implementation.
                    assert!(transfers
                        .created_utxos
                        .insert(location, Arc::new(utxo.clone()))
                        .is_none());
                    transfers.tx_ids.insert(*hash);
                }
            }
        }
    }
    assert_eq!(
        chain
            .created_utxos
            .iter()
            .map(|(key, value)| (*key, value.clone()))
            .collect::<HashMap<_, _>>(),
        created
    );
    assert_eq!(chain.spent_utxos, spent);
    assert_eq!(chain.tx_loc_by_hash, locations);
    assert_eq!(
        chain
            .partial_transparent_transfers
            .iter()
            .map(|(key, value)| (*key, value.clone()))
            .collect::<HashMap<_, _>>(),
        addresses
    );
    assert_eq!(chain.height_by_hash, heights);
    assert_eq!(chain.sprout_nullifiers, sprout);
    assert_eq!(chain.sapling_nullifiers, sapling);
    assert_eq!(chain.orchard_nullifiers, orchard);
    assert_eq!(chain.ironwood_nullifiers, ironwood);

    // A retained address history must share scripts with the created-output index, not copy them.
    for (outpoint, utxo) in chain.created_utxos.iter() {
        if let Some(address) = utxo.utxo.output.address(&chain.network) {
            let location = OutputLocation::from_outpoint(transaction_location(utxo), outpoint);
            let indexed = &chain
                .partial_transparent_transfers
                .get(&address)
                .unwrap()
                .created_utxos[&location];
            assert!(Arc::ptr_eq(utxo, indexed));
        }
    }
}

/// Regression test for #10556: cumulative receipts saturate rather than overflow.
#[test]
fn received_saturates_past_u64_max() {
    let max_money = Amount::try_from(MAX_MONEY).expect("MAX_MONEY is a valid amount");
    let output = Output::new(max_money, Script::new(&[]));
    let utxo = Arc::new(transparent::OrderedUtxo::new(output, Height(0), 0));
    let utxo_count = 8786;
    assert!(
        u128::try_from(utxo_count).unwrap() * u128::try_from(MAX_MONEY).unwrap()
            > u128::from(u64::MAX)
    );
    let mut transfers = TransparentTransfers::default();
    for output_index in 0..utxo_count {
        transfers.created_utxos.insert(
            OutputLocation::from_usize(Height(0), 0, output_index),
            utxo.clone(),
        );
    }
    assert_eq!(transfers.received(), u64::MAX);
}
