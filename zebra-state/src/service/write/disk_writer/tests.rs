//! Tests for the disk-writer stage of the block write pipeline.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use tokio::sync::oneshot;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};

use crate::{
    request::FinalizableBlock,
    service::{
        finalized_state::FinalizedState,
        write::disk_writer::{DiskRequest, DiskWriter},
        write::NO_DISK_TIP_HEIGHT,
    },
    CheckpointVerifiedBlock, Config,
};

/// Returns the first `count` mainnet blocks (genesis onward).
fn mainnet_blocks(count: u32) -> Vec<Arc<Block>> {
    let blocks = Network::Mainnet.blockchain_map();
    (0..count)
        .map(|height| {
            blocks
                .get(&height)
                .expect("test vectors include the first blocks of mainnet")
                .zcash_deserialize_into()
                .expect("test vectors deserialize")
        })
        .collect()
}

/// Spawns a [`DiskWriter`] over a fresh ephemeral database, and returns the
/// request sender, the shared `disk_tip_height`, a database handle, and the
/// writer's join handle.
fn spawn_disk_writer() -> (
    crossbeam_channel::Sender<DiskRequest>,
    Arc<AtomicU32>,
    crate::service::finalized_state::ZebraDb,
    std::thread::JoinHandle<()>,
) {
    let state = FinalizedState::new(
        &Config::ephemeral(),
        &Network::Mainnet,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    let db = state.db.clone();

    let (disk_tx, disk_rx) = crossbeam_channel::bounded::<DiskRequest>(500);
    let disk_tip_height = Arc::new(AtomicU32::new(NO_DISK_TIP_HEIGHT));

    let writer = DiskWriter {
        receiver: disk_rx,
        finalized_state: state,
        disk_tip_height: disk_tip_height.clone(),
    };
    let handle = std::thread::spawn(move || writer.run());

    (disk_tx, disk_tip_height, db, handle)
}

/// Sends one checkpoint block to the disk writer with a blocking ack, and
/// returns the result.
#[allow(clippy::unwrap_in_result)]
fn write_checkpoint_block(
    disk_tx: &crossbeam_channel::Sender<DiskRequest>,
    block: Arc<Block>,
    bulk: bool,
) -> Result<zebra_chain::block::Hash, crate::error::CommitCheckpointVerifiedError> {
    let (ack_tx, ack_rx) = oneshot::channel();
    disk_tx
        .send(DiskRequest::Write {
            block: Box::new(FinalizableBlock::Checkpoint {
                checkpoint_verified: CheckpointVerifiedBlock::from(block),
            }),
            bulk,
            ack: Some(ack_tx),
        })
        .expect("disk writer is alive");
    ack_rx.blocking_recv().expect("disk writer sent the ack")
}

/// A bulk write creates the guard (pauses auto-compaction); a later non-bulk
/// write drops it (resumes auto-compaction); a later bulk write re-creates it.
#[test]
fn guard_lifecycle_on_off_on() {
    let _init_guard = zebra_test::init();

    let (disk_tx, disk_tip_height, db, handle) = spawn_disk_writer();
    let blocks = mainnet_blocks(3);

    assert!(
        !db.auto_compaction_disabled(),
        "auto-compaction starts enabled",
    );

    // Bulk write (genesis): the guard is created, pausing auto-compaction.
    write_checkpoint_block(&disk_tx, blocks[0].clone(), true).expect("genesis commits");
    assert!(
        db.auto_compaction_disabled(),
        "a bulk write must create the guard and pause auto-compaction",
    );
    assert_eq!(disk_tip_height.load(Ordering::Acquire), 0);

    // Non-bulk write (block 1): the guard is dropped, resuming auto-compaction.
    write_checkpoint_block(&disk_tx, blocks[1].clone(), false).expect("block 1 commits");
    assert!(
        !db.auto_compaction_disabled(),
        "a non-bulk write must drop the guard and resume auto-compaction",
    );
    assert_eq!(disk_tip_height.load(Ordering::Acquire), 1);

    // Bulk write again (block 2): the guard is re-created.
    write_checkpoint_block(&disk_tx, blocks[2].clone(), true).expect("block 2 commits");
    assert!(
        db.auto_compaction_disabled(),
        "a later bulk write must re-create the guard",
    );
    assert_eq!(disk_tip_height.load(Ordering::Acquire), 2);

    // Closing the channel drops the guard on the way out.
    drop(disk_tx);
    handle.join().expect("disk writer exits cleanly");
    assert!(
        !db.auto_compaction_disabled(),
        "channel close must drop the guard and resume auto-compaction",
    );
}

/// An `EndBulk` message drops the guard mid-stream, and the next bulk write
/// re-creates it.
#[test]
fn end_bulk_drops_guard() {
    let _init_guard = zebra_test::init();

    let (disk_tx, _disk_tip_height, db, handle) = spawn_disk_writer();
    let blocks = mainnet_blocks(2);

    write_checkpoint_block(&disk_tx, blocks[0].clone(), true).expect("genesis commits");
    assert!(
        db.auto_compaction_disabled(),
        "bulk write pauses compaction"
    );

    disk_tx
        .send(DiskRequest::EndBulk)
        .expect("disk writer is alive");

    // EndBulk has no ack, so synchronize with a following bulk write whose ack
    // we wait on; by the time the ack arrives, EndBulk was processed first
    // (FIFO) and then the bulk write re-created the guard.
    write_checkpoint_block(&disk_tx, blocks[1].clone(), true).expect("block 1 commits");
    assert!(
        db.auto_compaction_disabled(),
        "the bulk write after EndBulk must re-create the guard",
    );

    drop(disk_tx);
    handle.join().expect("disk writer exits cleanly");
}

/// Each acked write returns the committed block's hash and advances the
/// published disk tip in commit order, so the worker's genesis and overflow
/// senders can rely on the ack for both the hash and durability.
#[test]
fn acked_writes_return_hashes_in_order() {
    let _init_guard = zebra_test::init();

    let (disk_tx, disk_tip_height, _db, handle) = spawn_disk_writer();
    let blocks = mainnet_blocks(3);

    for (height, block) in blocks.iter().enumerate() {
        let expected_hash = block.hash();
        // Genesis is bulk; the rest alternate, exercising both guard states.
        let bulk = height == 0 || height % 2 == 0;
        let hash = write_checkpoint_block(&disk_tx, block.clone(), bulk)
            .expect("test vector blocks commit");

        assert_eq!(hash, expected_hash, "the ack returns the committed hash");
        assert_eq!(
            disk_tip_height.load(Ordering::Acquire),
            height as u32,
            "the published disk tip advances after each acked write",
        );
    }

    drop(disk_tx);
    handle.join().expect("disk writer exits cleanly");
}
