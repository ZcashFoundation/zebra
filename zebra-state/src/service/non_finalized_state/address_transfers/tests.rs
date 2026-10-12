use super::*;
use crate::service::non_finalized_state::chain::{RevertPosition, UpdateWith};
use proptest::prelude::*;
use zebra_chain::{block::Height, parameters::NetworkKind, transaction, transparent};

fn address(id: u8) -> Address {
    Address::PayToPublicKeyHash {
        network_kind: NetworkKind::Mainnet,
        pub_key_hash: [id; 20],
    }
}

proptest! {
    #[test]
    fn histories_and_snapshots_match_hash_map(
        operations in prop::collection::vec((0u8..16, any::<bool>()), 1..128)
    ) {
        let mut index = AddressTransfers::default();
        let mut expected: HashMap<Address, TransparentTransfers> = HashMap::new();
        let mut outputs: HashMap<Address, Vec<(transparent::OutPoint, transparent::OrderedUtxo)>> = HashMap::new();
        let mut readers = Vec::new();
        for (step, (id, revert)) in operations.into_iter().enumerate() {
            let address = address(id);
            let retained = index.clone();
            let history = outputs.entry(address).or_default();
            if revert && !history.is_empty() {
                let (outpoint, utxo) = history.pop().unwrap();
                index.get_mut(&address).unwrap().revert_chain_with(&(&outpoint, &utxo), RevertPosition::Tip);
                expected.get_mut(&address).unwrap().revert_chain_with(&(&outpoint, &utxo), RevertPosition::Tip);
                if history.is_empty() {
                    prop_assert!(index.get(&address).unwrap().is_empty());
                    prop_assert_eq!(index.remove(&address).map(Arc::unwrap_or_clone), expected.remove(&address));
                }
            } else {
                let step = u32::try_from(step).unwrap();
                let mut hash = [0; 32];
                hash[..4].copy_from_slice(&step.to_le_bytes());
                let outpoint = transparent::OutPoint { hash: transaction::Hash(hash), index: 0 };
                let utxo = transparent::OrderedUtxo::new(
                    transparent::Output::new(1u64.try_into().unwrap(), transparent::Script::new(&[])),
                    Height(step),
                    0,
                );
                index.get_or_insert_mut(address).update_chain_tip_with(&(&outpoint, &utxo)).unwrap();
                expected.entry(address).or_default().update_chain_tip_with(&(&outpoint, &utxo)).unwrap();
                history.push((outpoint, utxo));
            }
            let touched = index.partition(&address);
            for (partition, (current, old)) in index.partitions.iter().zip(&retained.partitions).enumerate() {
                prop_assert_eq!(Arc::ptr_eq(current, old), partition != touched);
                if partition != touched {
                    continue;
                }
                for (other_address, old_history) in old.iter().filter(|(a, _)| **a != address) {
                    prop_assert!(Arc::ptr_eq(current.get(other_address).unwrap(), old_history));
                }
            }
            prop_assert_eq!(index.len(), expected.len());
            prop_assert_eq!(index.iter().map(|(a, t)| (*a, t.clone())).collect::<HashMap<_, _>>(), expected.clone());
            if step % 16 == 0 {
                readers.push((index.clone(), expected.clone()));
            }
        }
        let mut rebuilt = AddressTransfers::default();
        for (address, transfers) in &expected {
            *rebuilt.get_or_insert_mut(*address) = transfers.clone();
        }
        prop_assert_eq!(&index, &rebuilt);
        for (reader, reference) in readers {
            prop_assert_eq!(reader.len(), reference.len());
            for (address, transfers) in reference {
                prop_assert_eq!(reader.get(&address), Some(&transfers));
            }
        }
    }
}
