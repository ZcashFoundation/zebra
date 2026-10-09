use super::*;
use zebra_chain::{block::Height, transaction, transparent};

fn key(id: u32) -> OutPoint {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&id.to_le_bytes());
    OutPoint {
        hash: transaction::Hash(hash),
        index: id % 3,
    }
}

fn value(id: u32) -> Arc<OrderedUtxo> {
    Arc::new(OrderedUtxo::new(
        transparent::Output {
            value: u64::from(id).try_into().unwrap(),
            lock_script: transparent::Script::new(&[]),
        },
        Height(1),
        0,
    ))
}

#[test]
fn mutation_copies_only_the_touched_partition() {
    let mut index = CreatedUtxos::default();
    index.insert(key(0), value(0));
    let snapshot = index.clone();
    assert!(index
        .partitions
        .iter()
        .zip(&snapshot.partitions)
        .all(|(a, b)| Arc::ptr_eq(a, b)));

    index.insert(key(0), value(1));
    let touched = index.partition(&key(0));
    for (partition, (current, retained)) in index
        .partitions
        .iter()
        .zip(&snapshot.partitions)
        .enumerate()
    {
        assert_eq!(Arc::ptr_eq(current, retained), partition != touched);
    }
    assert_eq!(snapshot.get(&key(0)), Some(&value(0)));
    assert_eq!(index.get(&key(0)), Some(&value(1)));
    assert_eq!(index.len, 1);
}

#[test]
fn mutations_and_retained_snapshots_match_independent_maps() {
    let mut index = CreatedUtxos::default();
    let mut expected = HashMap::new();
    let mut readers = Vec::new();
    for step in 0..3000 {
        if step % 100 == 0 {
            readers.push((index.clone(), expected.clone()));
        }
        let key = key(step % 271);
        if step % 5 == 0 {
            assert_eq!(index.remove(&key), expected.remove(&key));
        } else {
            assert_eq!(
                index.insert(key, value(step)),
                expected.insert(key, value(step))
            );
        }
        assert_eq!(index.len, expected.len());
        assert_eq!(
            index
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect::<HashMap<_, _>>(),
            expected
        );
        for (reader, expected_reader) in &readers {
            assert_eq!(reader.len, expected_reader.len());
            for (key, value) in expected_reader {
                assert_eq!(reader.get(key), Some(value));
            }
        }
    }
    // Equality is by contents, even with independently randomized partition placement.
    let mut rebuilt = CreatedUtxos::default();
    for (key, value) in &expected {
        rebuilt.insert(*key, value.clone());
    }
    assert_eq!(index, rebuilt);
    for (key, value) in expected {
        assert_eq!(index.remove(&key), Some(value));
    }
    assert_eq!(index.len, 0);
    assert_eq!(index.iter().count(), 0);
    assert_ne!(index, rebuilt);
}
