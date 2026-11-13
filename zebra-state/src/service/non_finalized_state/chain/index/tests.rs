use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::service::non_finalized_state::Chain;
use zebra_chain::{
    amount::MAX_MONEY,
    block::Height,
    transparent::{Output, Script},
};

/// The transparent transfers of one address in a whole chain, replayed from its blocks.
#[derive(Debug, Default)]
struct ExpectedTransfers {
    balance: i128,
    received: u64,
    created: BTreeMap<OutputLocation, transparent::Output>,
    spent: BTreeSet<OutputLocation>,
    tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
}

/// Replay the pre-optimization non-tree indexes, including the allocating transparent accessors.
/// The caller also compares complete chain snapshots after tip rollback and reapplication.
pub(in crate::service::non_finalized_state) fn assert_legacy_indexes(chain: &Chain) {
    let mut created = HashMap::new();
    let mut spent = HashMap::new();
    let mut addresses: HashMap<transparent::Address, ExpectedTransfers> = HashMap::new();
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
                    transfers.balance -= i128::from(utxo.utxo.output.value().zatoshis());
                    let location =
                        OutputLocation::from_outpoint(transaction_location(utxo), &outpoint);
                    assert!(transfers.spent.insert(location));
                    transfers
                        .tx_ids
                        .insert(TransactionLocation::from_usize(block.height, index), *hash);
                }
            }
            for output_index in 0..outputs.len() {
                let outpoint = transparent::OutPoint {
                    hash: *hash,
                    index: output_index.try_into().unwrap(),
                };
                let utxo = block.new_outputs.get(&outpoint).unwrap();
                assert!(created
                    .insert(outpoint, Arc::new(utxo.as_ref().clone()))
                    .is_none());
                if let Some(address) = utxo.utxo.output.address(&chain.network) {
                    let transfers = addresses.entry(address).or_default();
                    let value = utxo.utxo.output.value();
                    transfers.balance += i128::from(value.zatoshis());
                    transfers.received = transfers.received.saturating_add(u64::from(value));
                    let location =
                        OutputLocation::from_outpoint(transaction_location(utxo), &outpoint);
                    assert!(transfers
                        .created
                        .insert(location, utxo.utxo.output.clone())
                        .is_none());
                    transfers
                        .tx_ids
                        .insert(TransactionLocation::from_usize(block.height, index), *hash);
                }
            }
        }
    }
    for (outpoint, utxo) in &created {
        assert_eq!(chain.created_ordered_utxo(outpoint), Some(utxo));
    }
    assert_eq!(chain.spent_utxos, spent);
    assert_eq!(chain.tx_loc_by_hash, locations);
    assert_eq!(chain.height_by_hash, heights);
    assert_eq!(chain.sprout_nullifiers, sprout);
    assert_eq!(chain.sapling_nullifiers, sapling);
    assert_eq!(chain.orchard_nullifiers, orchard);
    assert_eq!(chain.ironwood_nullifiers, ironwood);

    // Each address query matches the replay, for each address and for all of them at once.
    let all_heights = Height(0)..=Height::MAX;
    let mut all_balance = 0i128;
    let mut all_received = 0u64;
    for (address, expected) in &addresses {
        let address = HashSet::from([*address]);
        assert_eq!(
            chain.partial_transparent_balance_change_from(&address, Height(0)),
            (
                Amount::try_from(expected.balance).unwrap(),
                expected.received
            ),
        );
        assert_eq!(
            chain.partial_transparent_utxo_changes(&address),
            (expected.created.clone(), expected.spent.clone()),
        );
        assert_eq!(
            chain.partial_transparent_spent_utxo_count(&address),
            expected.spent.len()
        );
        assert_eq!(
            chain.partial_transparent_tx_ids(&address, all_heights.clone()),
            expected.tx_ids,
        );
        all_balance += expected.balance;
        all_received = all_received.saturating_add(expected.received);
    }
    let all_addresses: HashSet<_> = addresses.keys().copied().collect();
    assert_eq!(
        chain.partial_transparent_balance_change_from(&all_addresses, Height(0)),
        (Amount::try_from(all_balance).unwrap(), all_received),
    );

    // Querying more addresses than a block changes scans the block instead of looking each one
    // up, with the same results.
    let most_changed = chain
        .transparent_transfers_by_height
        .values()
        .map(|transfers| transfers.len())
        .max()
        .unwrap_or_default();
    let mut superset = all_addresses.clone();
    for i in 0..=u32::try_from(most_changed).unwrap() {
        let mut hash = [0xee; 20];
        hash[..4].copy_from_slice(&i.to_le_bytes());
        superset.insert(transparent::Address::from_script_hash(
            chain.network.kind(),
            hash,
        ));
    }
    assert_eq!(
        chain.partial_transparent_balance_change_from(&superset, Height(0)),
        chain.partial_transparent_balance_change_from(&all_addresses, Height(0)),
    );
    assert_eq!(
        chain.partial_transparent_utxo_changes(&superset),
        chain.partial_transparent_utxo_changes(&all_addresses),
    );
    assert_eq!(
        chain.partial_transparent_tx_ids(&superset, all_heights.clone()),
        chain.partial_transparent_tx_ids(&all_addresses, all_heights.clone()),
    );
    #[allow(clippy::reversed_empty_ranges)]
    let inverted = Height(1)..=Height(0);
    assert!(chain
        .partial_transparent_tx_ids(&superset, inverted)
        .is_empty());

    // Every block has address changes, and they share outputs with the block, not copy them.
    assert!(chain
        .transparent_transfers_by_height
        .keys()
        .eq(chain.blocks.keys()));
    for (height, transfers) in &chain.transparent_transfers_by_height {
        let block = &chain.blocks[height];
        for address in &all_addresses {
            let Some(transfers) = transfers.get(address) else {
                continue;
            };
            for (location, utxo) in transfers.created() {
                let outpoint = transparent::OutPoint {
                    hash: block.transaction_hashes[location.transaction_index().as_usize()],
                    index: location.output_index().index(),
                };
                assert!(Arc::ptr_eq(utxo, &block.new_outputs[&outpoint]));
            }
        }
    }
}

/// Regression test for #10556: cumulative receipts saturate rather than overflow.
#[test]
fn received_saturates_past_u64_max() {
    let address = transparent::Address::from_pub_key_hash(
        zebra_chain::parameters::NetworkKind::Mainnet,
        [7; 20],
    );
    let max_money = Amount::try_from(MAX_MONEY).expect("MAX_MONEY is a valid amount");
    let utxo = Arc::new(transparent::OrderedUtxo::new(
        Output::new(max_money, Script::new(&[])),
        Height(0),
        0,
    ));
    let block_count = 8786u32;
    assert!(u128::from(block_count) * u128::try_from(MAX_MONEY).unwrap() > u128::from(u64::MAX));

    let mut chain = Chain::default();
    for height in 0..block_count {
        let transfers = AddressBlockTransfers {
            balance: Amount::zero(),
            received: u64::from(max_money),
            created: vec![(
                OutputLocation::from_usize(Height(height), 0, 0),
                utxo.clone(),
            )],
            spent: Vec::new(),
            transactions: Vec::new(),
        };
        chain.transparent_transfers_by_height.insert(
            Height(height),
            Arc::new(BlockTransparentTransfers {
                by_address: HashMap::from([(address, transfers)]),
            }),
        );
    }

    let (_balance, received) =
        chain.partial_transparent_balance_change_from(&HashSet::from([address]), Height(0));
    assert_eq!(received, u64::MAX);
}
