use super::*;
use crate::service::non_finalized_state::chain::UpdateWith;
use zebra_chain::{
    block::Height,
    parameters::NetworkKind,
    transaction,
    transparent::{self, OrderedUtxo, OutPoint},
};

fn address(id: u32) -> Address {
    let mut pub_key_hash = [0; 20];
    pub_key_hash[..4].copy_from_slice(&id.to_le_bytes());
    Address::PayToPublicKeyHash {
        network_kind: NetworkKind::Mainnet,
        pub_key_hash,
    }
}

/// Records one received output in `transfers`, making each step's history distinct.
fn receive(transfers: &mut TransparentTransfers, step: u32) {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&step.to_le_bytes());
    let outpoint = OutPoint {
        hash: transaction::Hash(hash),
        index: 0,
    };
    let utxo = OrderedUtxo::new(
        transparent::Output {
            value: u64::from(step).try_into().unwrap(),
            lock_script: transparent::Script::new(&[]),
        },
        Height(step),
        0,
    );
    transfers
        .update_chain_tip_with(&(&outpoint, &utxo))
        .expect("each step receives a new output");
}

#[test]
fn mutation_copies_only_the_touched_partition() {
    let mut index = AddressTransfers::default();
    receive(index.get_or_insert_mut(address(0)), 0);
    let snapshot = index.clone();
    assert!(index
        .partitions
        .iter()
        .zip(&snapshot.partitions)
        .all(|(a, b)| Arc::ptr_eq(a, b)));

    receive(index.get_or_insert_mut(address(0)), 1);
    let touched = index.partition(&address(0));
    for (partition, (current, retained)) in index
        .partitions
        .iter()
        .zip(&snapshot.partitions)
        .enumerate()
    {
        assert_eq!(Arc::ptr_eq(current, retained), partition != touched);
    }

    let mut expected = TransparentTransfers::default();
    receive(&mut expected, 0);
    assert_eq!(snapshot.get(&address(0)), Some(&expected));
    receive(&mut expected, 1);
    assert_eq!(index.get(&address(0)), Some(&expected));
    assert_eq!(index.len(), 1);
}

#[test]
fn mutations_and_retained_snapshots_match_independent_maps() {
    let mut index = AddressTransfers::default();
    let mut expected: HashMap<Address, TransparentTransfers> = HashMap::new();
    let mut readers = Vec::new();
    for step in 0..3000 {
        if step % 100 == 0 {
            readers.push((index.clone(), expected.clone()));
        }
        let address = address(step % 271);
        if step % 5 == 0 {
            assert_eq!(
                index.remove(&address).map(Arc::unwrap_or_clone),
                expected.remove(&address)
            );
        } else if step % 7 == 0 {
            assert_eq!(
                index.get_mut(&address).is_some(),
                expected.contains_key(&address)
            );
            if let Some(transfers) = index.get_mut(&address) {
                receive(transfers, step);
                receive(expected.get_mut(&address).unwrap(), step);
            }
        } else {
            receive(index.get_or_insert_mut(address), step);
            receive(expected.entry(address).or_default(), step);
        }
        assert_eq!(index.len(), expected.len());
        assert_eq!(
            index
                .iter()
                .map(|(address, transfers)| (*address, transfers.clone()))
                .collect::<HashMap<_, _>>(),
            expected
        );
        for (reader, expected_reader) in &readers {
            assert_eq!(reader.len(), expected_reader.len());
            for (address, transfers) in expected_reader {
                assert_eq!(reader.get(address), Some(transfers));
            }
        }
    }
    // Equality is by contents, even with independently randomized partition placement.
    let mut rebuilt = AddressTransfers::default();
    for (address, transfers) in &expected {
        *rebuilt.get_or_insert_mut(*address) = transfers.clone();
    }
    assert_eq!(index, rebuilt);
    for (address, transfers) in expected {
        assert_eq!(
            index.remove(&address).map(Arc::unwrap_or_clone),
            Some(transfers)
        );
    }
    assert_eq!(index.len(), 0);
    assert_eq!(index.iter().count(), 0);
    assert_ne!(index, rebuilt);
}
