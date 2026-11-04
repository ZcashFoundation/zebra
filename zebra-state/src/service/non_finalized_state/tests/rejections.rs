//! Authenticated rejection metadata is shared by snapshots and pruned only by finalization.

use super::super::*;
use crate::{arbitrary::Prepare, tests::setup::new_state_with_mainnet_genesis};
use zebra_chain::serialization::ZcashDeserializeInto;

#[test]
fn rejected_hashes_survive_snapshot_replacement_until_finalized() {
    let _init_guard = zebra_test::init();
    let (finalized, mut writer, _) = new_state_with_mainnet_genesis();
    let first: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let second: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    writer
        .commit_new_chain(first.prepare(), &finalized.db)
        .unwrap();
    writer
        .commit_block(second.prepare(), &finalized.db)
        .unwrap();
    let snapshot = writer.clone();
    let rejected = Hash([1; 32]);
    let later = Hash([2; 32]);
    {
        let mut rejections = snapshot.rejected_blocks.lock().unwrap();
        rejections.reject(rejected, Height(2));
        rejections.reject(later, Height(3));
    }
    assert!(writer.rejected_blocks.lock().unwrap().contains(rejected));
    writer.finalize();
    assert!(snapshot.rejected_blocks.lock().unwrap().contains(rejected));
    writer.finalize();
    let mut rejections = snapshot.rejected_blocks.lock().unwrap();
    assert!(!rejections.contains(rejected));
    assert!(rejections.contains(later));
    rejections.reject(rejected, Height(2));
    assert!(
        !rejections.contains(rejected),
        "a late verifier cannot resurrect finalized metadata"
    );
}

#[test]
fn rejection_overflow_stays_closed_until_every_unrecorded_height_is_finalized() {
    let mut rejections = RejectedBlocks::default();
    for index in 0..MAX_EARLY_RELAY_REJECTIONS {
        let mut hash = Hash([0; 32]);
        hash.0[..4].copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
        assert_eq!(
            rejections.reject(hash, Height(1)),
            index + 1 == MAX_EARLY_RELAY_REJECTIONS,
        );
    }
    let overflow = Hash([255; 32]);
    let later = Hash([254; 32]);
    assert!(rejections.reject(overflow, Height(2)));
    assert!(rejections.reject(later, Height(3)));
    assert_eq!(rejections.hashes.len(), MAX_EARLY_RELAY_REJECTIONS);
    assert!(rejections.hashes.contains_key(&Hash([0; 32])));
    assert!(!rejections.prune(Height(1)));
    assert!(rejections.hashes.is_empty());
    assert!(rejections.is_full());
    assert!(
        !rejections.contains(overflow),
        "overflow does not claim an unknown hash is rejected"
    );
    assert!(!rejections.prune(Height(2)));
    assert!(rejections.is_full());
    assert!(rejections.prune(Height(3)));
    assert!(!rejections.is_full());
    assert!(
        !rejections.prune(Height(4)),
        "capacity clearing is signalled only once"
    );
}
