use super::*;
use proptest::prelude::*;
use zebra_chain::{block::Height, transaction, transparent};

fn key(id: u8) -> OutPoint {
    OutPoint {
        hash: transaction::Hash([id; 32]),
        index: u32::from(id),
    }
}

fn value(value: u32) -> Arc<OrderedUtxo> {
    Arc::new(OrderedUtxo::new(
        transparent::Output::new(
            u64::from(value)
                .try_into()
                .expect("test values are below MAX_MONEY"),
            transparent::Script::new(&[0; 1024]),
        ),
        Height(1),
        0,
    ))
}

proptest! {
    #[test]
    fn mutations_and_rollbacks_match_hash_map(
        operations in prop::collection::vec((any::<u8>(), any::<bool>(), 0u32..1000), 1..128)
    ) {
        let mut index = CreatedUtxos::default();
        let mut expected = HashMap::new();
        let mut readers = Vec::new();
        let mut undo = Vec::new();
        for (step, (id, remove, amount)) in operations.into_iter().enumerate() {
            let retained = index.clone();
            let outpoint = key(id);
            let touched = index.partition(&outpoint);
            let previous = if remove {
                let previous = expected.remove(&outpoint);
                prop_assert_eq!(index.remove(&outpoint), previous.clone());
                previous
            } else {
                let previous = expected.insert(outpoint, value(amount));
                prop_assert_eq!(index.insert(outpoint, value(amount)), previous.clone());
                previous
            };
            undo.push((outpoint, previous));
            for (partition, (current, old)) in index.partitions.iter().zip(&retained.partitions).enumerate() {
                prop_assert_eq!(Arc::ptr_eq(current, old), partition != touched);
            }
            prop_assert_eq!(index.len, expected.len());
            prop_assert_eq!(index.iter().map(|(k, v)| (*k, v.clone())).collect::<HashMap<_, _>>(), expected.clone());
            if step % 16 == 0 {
                readers.push((index.clone(), expected.clone()));
            }
        }
        let mut rebuilt = CreatedUtxos::default();
        for (outpoint, utxo) in &expected {
            rebuilt.insert(*outpoint, utxo.clone());
        }
        prop_assert_eq!(&index, &rebuilt);
        for (outpoint, previous) in undo.into_iter().rev() {
            if let Some(previous) = previous {
                index.insert(outpoint, previous.clone());
                expected.insert(outpoint, previous);
            } else {
                index.remove(&outpoint);
                expected.remove(&outpoint);
            }
            prop_assert_eq!(index.iter().map(|(k, v)| (*k, v.clone())).collect::<HashMap<_, _>>(), expected.clone());
        }
        prop_assert_eq!(index.len, 0);
        for (reader, reference) in readers {
            prop_assert_eq!(reader.len, reference.len());
            for (outpoint, utxo) in reference {
                prop_assert_eq!(reader.get(&outpoint), Some(&utxo));
            }
        }
    }
}
