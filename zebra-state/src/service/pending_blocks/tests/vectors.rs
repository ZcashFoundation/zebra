//! Fixed test vectors for block queues.

use std::sync::Arc;

use tokio::sync::oneshot;

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::pending_blocks::{PendingBlocks, QueuedNonFinalized},
    tests::FakeChainHelper,
};

// Quick helper trait for making queued blocks with throw away channels
trait IntoQueued {
    fn into_queued(self) -> QueuedNonFinalized;
}

impl IntoQueued for Arc<Block> {
    fn into_queued(self) -> QueuedNonFinalized {
        let (rsp_tx, _) = oneshot::channel();
        (self.prepare(), rsp_tx)
    }
}

#[test]
fn dequeue_gives_right_children() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block1: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let child1: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let child2 = block1.make_fake_child();

    let parent = block1.header.previous_block_hash;

    let mut pending = PendingBlocks::default();
    // Empty to start
    assert_eq!(0, pending.queued.blocks.len());
    assert_eq!(0, pending.queued.by_parent.len());
    assert_eq!(0, pending.queued.by_height.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // Inserting the first block gives us 1 in each table, and some UTXOs
    pending.queue(block1.clone().into_queued());
    assert_eq!(1, pending.queued.blocks.len());
    assert_eq!(1, pending.queued.by_parent.len());
    assert_eq!(1, pending.queued.by_height.len());
    assert_eq!(2, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // The second gives us another in each table because its a child of the first,
    // and a lot of UTXOs
    pending.queue(child1.clone().into_queued());
    assert_eq!(2, pending.queued.blocks.len());
    assert_eq!(2, pending.queued.by_parent.len());
    assert_eq!(2, pending.queued.by_height.len());
    assert_eq!(632, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // The 3rd only increments blocks, because it is also a child of the
    // first block, so for the second and third tables it gets added to the
    // existing HashSet value
    pending.queue(child2.clone().into_queued());
    assert_eq!(3, pending.queued.blocks.len());
    assert_eq!(2, pending.queued.by_parent.len());
    assert_eq!(2, pending.queued.by_height.len());
    assert_eq!(634, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // Dequeueing the first block removes 1 block from each list
    let children = pending
        .dequeue_children(parent)
        .next()
        .expect("should be at least one set of children dequeued");
    assert_eq!(1, children.len());
    assert_eq!(block1, children[0].0.block);
    assert_eq!(2, pending.queued.blocks.len());
    assert_eq!(1, pending.queued.by_parent.len());
    assert_eq!(1, pending.queued.by_height.len());
    assert_eq!(634, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(1, pending.sent.curr_buf.len());
    assert_eq!(1, pending.sent.sent.len());

    // Dequeueing the children of the first block removes both of the other
    // blocks, and empties all lists
    let parent = children[0].0.block.hash();
    let children = pending
        .dequeue_children(parent)
        .next()
        .expect("should be at least one set of children dequeued");
    assert_eq!(2, children.len());
    assert!(children
        .iter()
        .any(|(block, _)| block.hash == child1.hash()));
    assert!(children
        .iter()
        .any(|(block, _)| block.hash == child2.hash()));
    assert_eq!(0, pending.queued.blocks.len());
    assert_eq!(0, pending.queued.by_parent.len());
    assert_eq!(0, pending.queued.by_height.len());
    assert_eq!(634, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(3, pending.sent.curr_buf.len());
    assert_eq!(3, pending.sent.sent.len());

    Ok(())
}

#[test]
fn prune_removes_right_children() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block1: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let child1: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let child2 = block1.make_fake_child();

    let mut pending = PendingBlocks::default();
    pending.queue(block1.clone().into_queued());
    pending.queue(child1.clone().into_queued());
    pending.queue(child2.clone().into_queued());
    assert_eq!(3, pending.queued.blocks.len());
    assert_eq!(2, pending.queued.by_parent.len());
    assert_eq!(2, pending.queued.by_height.len());
    assert_eq!(634, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // Pruning the first height removes only block1
    pending.prune_by_height(block1.coinbase_height().unwrap());
    assert_eq!(2, pending.queued.blocks.len());
    assert_eq!(1, pending.queued.by_parent.len());
    assert_eq!(1, pending.queued.by_height.len());
    assert!(pending.queued.get_mut(&block1.hash()).is_none());
    assert!(pending.queued.get_mut(&child1.hash()).is_some());
    assert!(pending.queued.get_mut(&child2.hash()).is_some());
    assert_eq!(632, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    // Pruning the children of the first block removes both of the other
    // blocks, and empties all lists
    pending.prune_by_height(child1.coinbase_height().unwrap());
    assert_eq!(0, pending.queued.blocks.len());
    assert_eq!(0, pending.queued.by_parent.len());
    assert_eq!(0, pending.queued.by_height.len());
    assert!(pending.queued.get_mut(&child1.hash()).is_none());
    assert!(pending.queued.get_mut(&child2.hash()).is_none());
    assert_eq!(0, pending.known_utxos.len());
    assert_eq!(0, pending.sent.bufs.len());
    assert_eq!(0, pending.sent.curr_buf.len());
    assert_eq!(0, pending.sent.sent.len());

    Ok(())
}
