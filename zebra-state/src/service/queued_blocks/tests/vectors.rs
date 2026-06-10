//! Fixed test vectors for block queues.

use std::sync::Arc;

use tokio::sync::oneshot;

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::queued_blocks::{QueuedBlocks, QueuedSemanticallyVerified, SentHashes},
    tests::FakeChainHelper,
};

// Quick helper trait for making queued blocks with throw away channels
trait IntoQueued {
    fn into_queued(self) -> QueuedSemanticallyVerified;
}

impl IntoQueued for Arc<Block> {
    fn into_queued(self) -> QueuedSemanticallyVerified {
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

    let mut queue = QueuedBlocks::default();
    // Empty to start
    assert_eq!(0, queue.blocks.len());
    assert_eq!(0, queue.by_parent.len());
    assert_eq!(0, queue.by_height.len());
    assert_eq!(0, queue.known_utxos.len());

    // Inserting the first block gives us 1 in each table, and some UTXOs
    queue.queue(block1.clone().into_queued());
    assert_eq!(1, queue.blocks.len());
    assert_eq!(1, queue.by_parent.len());
    assert_eq!(1, queue.by_height.len());
    assert_eq!(2, queue.known_utxos.len());

    // The second gives us another in each table because its a child of the first,
    // and a lot of UTXOs
    queue.queue(child1.clone().into_queued());
    assert_eq!(2, queue.blocks.len());
    assert_eq!(2, queue.by_parent.len());
    assert_eq!(2, queue.by_height.len());
    assert_eq!(632, queue.known_utxos.len());

    // The 3rd only increments blocks, because it is also a child of the
    // first block, so for the second and third tables it gets added to the
    // existing HashSet value
    queue.queue(child2.clone().into_queued());
    assert_eq!(3, queue.blocks.len());
    assert_eq!(2, queue.by_parent.len());
    assert_eq!(2, queue.by_height.len());
    assert_eq!(634, queue.known_utxos.len());

    // Dequeueing the first block removes 1 block from each list
    let children = queue.dequeue_children(parent);
    assert_eq!(1, children.len());
    assert_eq!(block1, children[0].0.block);
    assert_eq!(2, queue.blocks.len());
    assert_eq!(1, queue.by_parent.len());
    assert_eq!(1, queue.by_height.len());
    assert_eq!(632, queue.known_utxos.len());

    // Dequeueing the children of the first block removes both of the other
    // blocks, and empties all lists
    let parent = children[0].0.block.hash();
    let children = queue.dequeue_children(parent);
    assert_eq!(2, children.len());
    assert!(children
        .iter()
        .any(|(block, _)| block.hash == child1.hash()));
    assert!(children
        .iter()
        .any(|(block, _)| block.hash == child2.hash()));
    assert_eq!(0, queue.blocks.len());
    assert_eq!(0, queue.by_parent.len());
    assert_eq!(0, queue.by_height.len());
    assert_eq!(0, queue.known_utxos.len());

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

    let mut queue = QueuedBlocks::default();
    queue.queue(block1.clone().into_queued());
    queue.queue(child1.clone().into_queued());
    queue.queue(child2.clone().into_queued());
    assert_eq!(3, queue.blocks.len());
    assert_eq!(2, queue.by_parent.len());
    assert_eq!(2, queue.by_height.len());
    assert_eq!(634, queue.known_utxos.len());

    // Pruning the first height removes only block1
    queue.prune_by_height(block1.coinbase_height().unwrap());
    assert_eq!(2, queue.blocks.len());
    assert_eq!(1, queue.by_parent.len());
    assert_eq!(1, queue.by_height.len());
    assert!(queue.get_mut(&block1.hash()).is_none());
    assert!(queue.get_mut(&child1.hash()).is_some());
    assert!(queue.get_mut(&child2.hash()).is_some());
    assert_eq!(632, queue.known_utxos.len());

    // Pruning the children of the first block removes both of the other
    // blocks, and empties all lists
    queue.prune_by_height(child1.coinbase_height().unwrap());
    assert_eq!(0, queue.blocks.len());
    assert_eq!(0, queue.by_parent.len());
    assert_eq!(0, queue.by_height.len());
    assert!(queue.get_mut(&child1.hash()).is_none());
    assert!(queue.get_mut(&child2.hash()).is_none());
    assert_eq!(0, queue.known_utxos.len());

    Ok(())
}

/// `SentHashes::remove` must drop the hash, its outpoints from `known_utxos`,
/// and the corresponding `(hash, height)` entry from `curr_buf` (or whichever
/// batch buffer holds it). Without this, a rejected same-hash block would
/// keep a later honest re-delivery of a block at the same hash locked out as
/// a "duplicate" forever.
#[test]
fn sent_hashes_remove_drops_rejected_hash_and_utxos() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block1: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let block2: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;

    let prepared1 = block1.clone().prepare();
    let prepared2 = block2.clone().prepare();

    let mut sent = SentHashes::default();
    sent.add(&prepared1);
    sent.add(&prepared2);

    // Both hashes are present, and `known_utxos` contains every outpoint from
    // both blocks' coinbase + transparent outputs.
    let utxos_after_add = sent.known_utxos.len();
    assert!(sent.contains(&prepared1.hash));
    assert!(sent.contains(&prepared2.hash));
    assert!(utxos_after_add > 0);

    // Remove block1. block1's hash disappears, block2's stays, and the
    // total number of known utxos shrinks by exactly block1's contribution.
    let block1_utxos = prepared1.new_outputs.len();
    sent.remove(&prepared1.hash);

    assert!(
        !sent.contains(&prepared1.hash),
        "removed hash must not satisfy contains()"
    );
    assert!(sent.contains(&prepared2.hash));
    assert_eq!(
        sent.known_utxos.len(),
        utxos_after_add - block1_utxos,
        "remove must drop only the removed block's outpoints"
    );

    // The (hash, height) entry must be gone from the batch buffer too,
    // otherwise a later `prune_by_height` could re-insert into `sent`.
    assert!(
        !sent.curr_buf.iter().any(|(h, _)| h == &prepared1.hash),
        "remove must drop the (hash, height) entry from curr_buf"
    );
    assert!(sent.curr_buf.iter().any(|(h, _)| h == &prepared2.hash));

    // Removing a hash that isn't tracked is a no-op.
    let block3 = block1.make_fake_child();
    sent.remove(&block3.hash());
    assert!(sent.contains(&prepared2.hash));

    Ok(())
}

// Ensures `dequeue_children` does not remove same-height sibling blocks from other forks.
#[test]
fn dequeue_children_preserves_same_height_siblings() -> Result<()> {
    let _init_guard = zebra_test::init();

    let root_block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;

    let left_child: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let left_grandchild = left_child.make_fake_child();

    let right_child = root_block.make_fake_child();
    let right_grandchild = right_child.make_fake_child();

    let mut queue = QueuedBlocks::default();
    queue.queue(left_grandchild.clone().into_queued());
    queue.queue(right_grandchild.clone().into_queued());

    let height = left_grandchild.coinbase_height().unwrap();

    // Sanity check: both entries are indexed under the same height bucket
    assert_eq!(
        queue.by_height.get(&height).unwrap().len(),
        2,
        "expected both fork grandchildren to be in the same height bucket"
    );

    // Dequeue only one branch
    queue.dequeue_children(left_child.hash());

    assert!(
        queue.blocks.contains_key(&right_grandchild.hash()),
        "sibling block must remain in queue after unrelated dequeue"
    );

    assert!(
        queue
            .by_height
            .get(&height)
            .unwrap()
            .contains(&right_grandchild.hash()),
        "sibling must remain indexed by height after unrelated dequeue"
    );

    Ok(())
}
