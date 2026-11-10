use super::*;
use crate::service::non_finalized_state::Chain;
use zebra_chain::{
    amount::MAX_MONEY,
    transparent::{Output, Script},
};

/// Keep the flat created-output map as an independent reconstruction oracle.
#[derive(Default)]
struct LegacyTransfers {
    balance: Amount<NegativeAllowed>,
    tx_ids: MultiSet<transaction::Hash>,
    created_utxos: BTreeMap<OutputLocation, Arc<transparent::OrderedUtxo>>,
    spent_utxos: BTreeSet<OutputLocation>,
}

/// Replay the pre-optimization non-tree indexes, including the allocating transparent accessors.
/// The caller also compares complete chain snapshots after tip rollback and reapplication.
pub(in crate::service::non_finalized_state) fn assert_legacy_indexes(chain: &Chain) {
    let mut created = HashMap::new();
    let mut spent = HashMap::new();
    let mut addresses: HashMap<transparent::Address, LegacyTransfers> = HashMap::new();
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
    assert_eq!(chain.partial_transparent_transfers.len(), addresses.len());
    for (address, legacy) in &addresses {
        let current = chain.partial_transparent_transfers.get(address).unwrap();
        assert_eq!(current.balance(), legacy.balance);
        assert_eq!(current.tx_ids, legacy.tx_ids);
        assert_eq!(current.spent_utxos(), &legacy.spent_utxos);
        assert_eq!(
            current.created_utxos().collect::<Vec<_>>(),
            legacy
                .created_utxos
                .iter()
                .map(|(location, utxo)| (location, &utxo.utxo.output))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            current.received(),
            legacy
                .created_utxos
                .values()
                .map(|utxo| u64::from(utxo.utxo.output.value()))
                .fold(0, u64::saturating_add)
        );
    }
    assert_eq!(chain.height_by_hash, heights);
    assert_eq!(chain.sprout_nullifiers, sprout);
    assert_eq!(chain.sapling_nullifiers, sapling);
    assert_eq!(chain.orchard_nullifiers, orchard);
    assert_eq!(chain.ironwood_nullifiers, ironwood);

    // A retained address history must share scripts with the created-output index, not copy them.
    for (outpoint, utxo) in chain.created_utxos.iter() {
        if let Some(address) = utxo.utxo.output.address(&chain.network) {
            let location = OutputLocation::from_outpoint(transaction_location(utxo), outpoint);
            let indexed = chain
                .partial_transparent_transfers
                .get(&address)
                .unwrap()
                .created_utxos[&location.height()]
                .get(&location)
                .unwrap();
            assert!(Arc::ptr_eq(utxo, indexed));
        }
    }
}

#[test]
fn singleton_address_history_stays_within_flat_map_allocation() {
    use crate::service::read::tests::spent_outputs::measure;
    let _init_guard = zebra_test::init();

    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([0; 32]),
        index: 0,
    };
    let utxo = Arc::new(transparent::OrderedUtxo::new(
        Output::new(Amount::zero(), Script::new(&[])),
        Height(1),
        0,
    ));
    let (legacy, flat_heap) = measure(|| {
        let mut legacy = LegacyTransfers::default();
        legacy.created_utxos.insert(
            OutputLocation::from_outpoint(transaction_location(&utxo), &outpoint),
            utxo.clone(),
        );
        legacy.tx_ids.insert(outpoint.hash);
        legacy
    });
    let (transfers, heap) = measure(|| {
        let mut transfers = TransparentTransfers::default();
        transfers
            .update_chain_tip_with(&(&outpoint, &utxo))
            .unwrap();
        transfers
    });
    tracing::info!(
        "singleton heap: previous={} current={}; struct: previous={} current={}",
        flat_heap.total,
        heap.total,
        std::mem::size_of_val(&legacy),
        std::mem::size_of_val(&transfers),
    );
    assert!(
        heap.total <= flat_heap.total,
        "a singleton address must not allocate an inner tree"
    );
    assert_eq!(transfers.created_utxos().count(), 1);
}

#[test]
fn shared_height_bucket_returns_to_singleton_after_revert() {
    for position in [RevertPosition::Root, RevertPosition::Tip] {
        let outpoints = [0, 1].map(|index| transparent::OutPoint {
            hash: transaction::Hash([0; 32]),
            index,
        });
        let utxo = Arc::new(transparent::OrderedUtxo::new(
            Output::new(Amount::zero(), Script::new(&[])),
            Height(1),
            0,
        ));
        let mut transfers = TransparentTransfers::default();
        for outpoint in &outpoints {
            transfers.update_chain_tip_with(&(outpoint, &utxo)).unwrap();
        }
        let retained = transfers.clone();
        transfers.revert_chain_with(&(&outpoints[0], &utxo), position);
        assert!(matches!(
            transfers.created_utxos[&Height(1)].as_ref(),
            CreatedOutputs::Single(..)
        ));
        assert_eq!(transfers.created_utxos().count(), 1);
        assert_eq!(retained.created_utxos().count(), 2);
        assert!(Arc::ptr_eq(
            transfers.created_utxos[&Height(1)].iter().next().unwrap().1,
            &utxo
        ));
        transfers.revert_chain_with(&(&outpoints[1], &utxo), position);
        assert!(transfers.is_empty());
    }
}

#[test]
fn sparse_height_bucket_allocation_is_bounded() {
    use crate::service::read::tests::spent_outputs::measure;
    let _init_guard = zebra_test::init();

    let utxo = Arc::new(transparent::OrderedUtxo::new(
        Output::new(Amount::zero(), Script::new(&[])),
        Height(1),
        0,
    ));
    let (_, singleton) = measure(|| {
        let mut outputs = BTreeMap::new();
        outputs.insert(OutputLocation::from_usize(Height(0), 0, 0), utxo.clone());
        outputs
    });
    for heights in [1, 5, 11, 12, 1000] {
        for outputs_per_height in [1, 2, 11, 12] {
            let (legacy, previous) = measure(|| {
                let mut outputs = BTreeMap::new();
                for height in 0..heights {
                    for index in 0..outputs_per_height {
                        outputs.insert(
                            OutputLocation::from_usize(Height(height), 0, index),
                            utxo.clone(),
                        );
                    }
                }
                outputs
            });
            let (buckets, current) = measure(|| {
                let mut buckets: BTreeMap<Height, Arc<CreatedOutputs>> = BTreeMap::new();
                for height in 0..heights {
                    for index in 0..outputs_per_height {
                        Arc::make_mut(buckets.entry(Height(height)).or_default()).insert(
                            OutputLocation::from_usize(Height(height), 0, index),
                            utxo.clone(),
                        );
                    }
                }
                buckets
            });
            tracing::info!(
                "heights={heights} outputs_per_height={outputs_per_height} previous={} current={}",
                previous.total,
                current.total,
            );
            assert!(
                current.total <= singleton.total * legacy.len(),
                "buckets must stay below the flat layout's singleton-per-output worst case"
            );
            assert_eq!(
                buckets
                    .values()
                    .flat_map(|outputs| outputs.iter())
                    .collect::<Vec<_>>(),
                legacy.iter().collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn reverting_shared_singleton_height_does_not_allocate() {
    use crate::service::read::tests::spent_outputs::measure;

    for position in [RevertPosition::Root, RevertPosition::Tip] {
        let outpoint = transparent::OutPoint {
            hash: transaction::Hash([0; 32]),
            index: 0,
        };
        let utxo = Arc::new(transparent::OrderedUtxo::new(
            Output::new(1u64.try_into().unwrap(), Script::new(&[])),
            Height(1),
            0,
        ));
        let mut transfers = TransparentTransfers::default();
        transfers
            .update_chain_tip_with(&(&outpoint, &utxo))
            .unwrap();
        let retained = transfers.clone();
        let ((), heap) = measure(|| transfers.revert_chain_with(&(&outpoint, &utxo), position));
        assert_eq!(
            heap.total, 0,
            "removing a shared singleton must not copy it"
        );
        assert!(transfers.is_empty());
        assert_eq!(retained.created_utxos().count(), 1);
        assert_eq!(retained.received(), 1);
    }
}

/// Regression test for #10556: cumulative receipts saturate rather than overflow.
#[test]
fn received_saturates_past_u64_max() {
    let max_money = Amount::try_from(MAX_MONEY).expect("MAX_MONEY is a valid amount");
    let output = Output::new(max_money, Script::new(&[]));
    let utxos = [0, 1].map(|height| {
        Arc::new(transparent::OrderedUtxo::new(
            output.clone(),
            Height(height),
            0,
        ))
    });
    let utxo_count = 8786;
    assert!(
        u128::try_from(utxo_count).unwrap() * u128::try_from(MAX_MONEY).unwrap()
            > u128::from(u64::MAX)
    );
    let mut transfers = TransparentTransfers::default();
    for output_index in 0..utxo_count {
        let utxo = &utxos[output_index % 2];
        let height = utxo.utxo.height;
        Arc::make_mut(transfers.created_utxos.entry(height).or_default()).insert(
            OutputLocation::from_usize(height, 0, output_index),
            utxo.clone(),
        );
    }
    assert_eq!(transfers.received(), u64::MAX);
}
