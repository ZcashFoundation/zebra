//! Format checks preserve permanent metadata checks while their body window is pruned.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    block_info::BlockInfo,
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_db::service::finalized_state::disk_format::upgrade::block_info_and_address_received::tests::AFTER_CAPTURED_TIP;

use crate::{
    service::finalized_state::disk_format::upgrade::DiskFormatUpgrade, CheckpointVerifiedBlock,
    Config, DiskWriteBatch, FinalizedState, TransactionLocation, WriteDisk, ZebraDb,
};

use super::*;

fn state_with_recent_window() -> FinalizedState {
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Network::Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=5) {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(
                    bytes.zcash_deserialize_into::<Arc<Block>>().unwrap(),
                )
                .into(),
                None,
                "format check pruning race",
            )
            .unwrap();
    }

    // Extend only the permanent metadata read by this check to a 1,001-block window.
    // The real transactions and received balances above exercise its address validation.
    let db = state.db.clone();
    let info = db.block_info_cf().zs_get(&Height(5)).unwrap();
    let hash = db.hash(Height(5)).unwrap();
    let mut batch = DiskWriteBatch::new();
    for height in 6..=1_001 {
        let _ = db
            .block_info_cf()
            .with_batch_for_writing(&mut batch)
            .zs_insert(&Height(height), &info);
    }
    batch.zs_insert(
        &db.cf_handle("hash_by_height").unwrap(),
        Height(1_001),
        hash,
    );
    db.write_batch(batch).unwrap();
    state
}

fn prune_next_window(db: &ZebraDb) {
    let tip = db.finalized_tip_height().unwrap();
    let next = (tip + 1).unwrap();
    let info = db.block_info_cf().zs_get(&tip).unwrap();
    let hash = db.hash(tip).unwrap();
    let mut batch = DiskWriteBatch::new();
    let _ = db
        .block_info_cf()
        .with_batch_for_writing(&mut batch)
        .zs_insert(&next, &info);
    batch.zs_insert(&db.cf_handle("hash_by_height").unwrap(), next, hash);
    // Match an atomic finalization/pruning batch with a retention of 1,001 blocks.
    batch.zs_delete_range(
        &db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height((tip - 1_000).unwrap()),
        TransactionLocation::min_for_height((next - 1_000).unwrap()),
    );
    db.write_batch(batch).unwrap();
}

#[test]
fn recent_checks_start_at_the_snapshot_window() {
    use crate::tests::snapshot_db::{snapshot_state, Coinbase};

    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(cache.path().to_path_buf(), 60, 21, Coinbase::Branched, 0);
    let db = &state.db;
    assert!(db.block_info_cf().zs_get(&Height(1)).is_none());
    assert!(db
        .block_info_cf()
        .zs_get(&base.window_start.previous().unwrap())
        .is_some());

    let (_sender, receiver) = crossbeam_channel::bounded(1);
    assert_eq!(Upgrade.validate(db, &receiver).unwrap(), Ok(()));
}

#[test]
fn recent_address_checks_use_one_pass_during_concurrent_pruning() {
    let _guard = zebra_test::init();
    let state = state_with_recent_window();
    let db = &state.db;
    let writer = db.clone();
    AFTER_CAPTURED_TIP.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || prune_next_window(&writer)));
    });

    let (_sender, receiver) = crossbeam_channel::bounded(1);
    let result = Upgrade.validate(db, &receiver).unwrap();
    AFTER_CAPTURED_TIP.with(|hook| hook.borrow_mut().take());
    assert_eq!(result, Ok(()));
    assert_eq!(db.finalized_tip_height(), Some(Height(1_002)));
    assert_eq!(
        db.history_capabilities().unwrap().lowest_retained_height,
        Height(2)
    );
}

#[test]
fn recent_address_checks_preserve_corruption_after_concurrent_pruning() {
    let _guard = zebra_test::init();
    for zero_info in [false, true] {
        let state = state_with_recent_window();
        let db = &state.db;
        let mut batch = DiskWriteBatch::new();
        let info_cf = db.block_info_cf().with_batch_for_writing(&mut batch);
        if zero_info {
            let _ = info_cf.zs_insert(&Height(1), &BlockInfo::default());
        } else {
            let _ = info_cf.zs_delete(&Height(1));
        }
        db.write_batch(batch).unwrap();

        let writer = db.clone();
        AFTER_CAPTURED_TIP.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if writer.finalized_tip_height() == Some(Height(1_001)) {
                    prune_next_window(&writer);
                }
            }));
        });

        let (_sender, receiver) = crossbeam_channel::bounded(1);
        let result = Upgrade.validate(db, &receiver).unwrap();
        AFTER_CAPTURED_TIP.with(|hook| hook.borrow_mut().take());
        let kind = if zero_info { "zero" } else { "missing" };
        assert_eq!(result, Err(format!("{kind} block info for height: 1")));
        assert_eq!(db.finalized_tip_height(), Some(Height(1_002)));
        assert_eq!(
            db.history_capabilities().unwrap().lowest_retained_height,
            Height(2)
        );
    }
}
