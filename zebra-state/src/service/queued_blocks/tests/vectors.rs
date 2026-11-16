//! Fixed test vectors for block queues.

use std::sync::Arc;

use tokio::sync::oneshot::{self, error::TryRecvError};

use zebra_chain::{
    amount::Amount,
    block::{self, Block},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::queued_blocks::{
        QueuedBlocks, QueuedSemanticallyVerified, SentHashes, MAX_QUEUED_BLOCKS, MAX_QUEUED_BYTES,
    },
    tests::FakeChainHelper,
    CommitBlockError,
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

/// Blocks with the same transactions share outpoints, so removing one of them from the
/// sent blocks must keep the shared outpoints known until the other is removed too.
#[test]
fn sent_hashes_keep_shared_outpoints_until_last_owner_is_removed() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let first = block.clone().prepare();
    let second = block.set_block_commitment([1; 32]).prepare();
    let shared = *first.new_outputs.keys().next().expect("block has outputs");
    assert!(second.new_outputs.contains_key(&shared));

    let mut sent = SentHashes::default();
    sent.add(&first);
    sent.add(&second);

    sent.remove(&first.hash);
    assert!(sent.utxo(&shared).is_some());

    sent.remove(&second.hash);
    assert!(sent.utxo(&shared).is_none());

    Ok(())
}

/// Like `sent_hashes_keep_shared_outpoints_until_last_owner_is_removed`, for queued blocks
/// with different parents.
#[test]
fn queued_blocks_keep_shared_outpoints_until_last_owner_is_dequeued() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let mut other = Block::clone(&block);
    Arc::make_mut(&mut other.header).previous_block_hash = block::Hash([1; 32]);
    let other = Arc::new(other);

    let shared = *block
        .clone()
        .prepare()
        .new_outputs
        .keys()
        .next()
        .expect("block has outputs");

    let mut queue = QueuedBlocks::default();
    queue.queue(block.clone().into_queued());
    queue.queue(other.clone().into_queued());

    queue.dequeue_children(block.header.previous_block_hash);
    assert!(queue.utxo(&shared).is_some());

    queue.dequeue_children(other.header.previous_block_hash);
    assert!(queue.utxo(&shared).is_none());

    Ok(())
}

/// Adding a sent block again releases its earlier outpoints, so removing it once forgets them.
#[test]
fn sent_hashes_readd_does_not_keep_outpoints() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.zcash_deserialize_into()?;
    let prepared = block.prepare();
    let outpoint = *prepared
        .new_outputs
        .keys()
        .next()
        .expect("block has outputs");

    let mut sent = SentHashes::default();
    sent.add(&prepared);
    sent.add(&prepared);

    sent.remove(&prepared.hash);
    assert!(sent.utxo(&outpoint).is_none());

    Ok(())
}

/// A full queue fails its highest blocks, so a peer can't fill it with blocks whose parents never
/// arrive. It keeps the lower blocks, because their parents are needed first.
#[test]
fn full_queue_fails_highest_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let mut queue = QueuedBlocks::default();
    let mut receivers = Vec::new();
    for _ in 0..=MAX_QUEUED_BLOCKS {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        queue.queue((block.clone().prepare(), rsp_tx));
        receivers.push(rsp_rx);
        block = block.make_fake_child();
    }

    queue.prune_overflow();

    assert_eq!(queue.blocks.len(), MAX_QUEUED_BLOCKS);
    assert_eq!(queue.by_height.len(), MAX_QUEUED_BLOCKS);
    assert_eq!(queue.by_parent.len(), MAX_QUEUED_BLOCKS);
    assert_eq!(
        queue.queued_bytes,
        queue
            .blocks
            .values()
            .map(|(_, bytes)| *bytes)
            .sum::<usize>()
    );
    assert_eq!(
        receivers.pop().expect("blocks were queued").try_recv(),
        Ok(Err(CommitBlockError::OrphanQueueFull.into()))
    );
    assert!(receivers
        .iter_mut()
        .all(|rsp_rx| rsp_rx.try_recv() == Err(TryRecvError::Empty)));

    Ok(())
}

/// A block with many small outputs uses many times its serialized size in memory, mostly in
/// its outputs and their copies. The queue limits that memory, not just the number of blocks.
#[test]
fn queue_limits_memory_of_blocks_with_many_small_outputs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let mut block = Block::clone(&block);
    let output = transparent::Output {
        value: Amount::zero(),
        lock_script: transparent::Script::new(&[]),
    };
    let coinbase = (*block.transactions[0])
        .clone()
        .with_transparent_outputs(vec![output; 100_000]);
    block.transactions[0] = Arc::new(coinbase);
    let mut block = Arc::new(block);
    assert!(
        block.clone().prepare().retained_bytes() > 10 * block.zcash_serialized_size(),
        "the memory estimate counts the decoded outputs, not just the serialized block"
    );

    let mut queue = QueuedBlocks::default();
    let mut receivers = Vec::new();
    while queue.queued_bytes <= MAX_QUEUED_BYTES {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        queue.queue((block.clone().prepare(), rsp_tx));
        receivers.push(rsp_rx);
        block = block.make_fake_child();
    }

    queue.prune_overflow();

    assert!(queue.queued_bytes <= MAX_QUEUED_BYTES);
    assert_eq!(
        receivers.pop().expect("blocks were queued").try_recv(),
        Ok(Err(CommitBlockError::OrphanQueueFull.into()))
    );
    assert_eq!(queue.blocks.len(), receivers.len());

    // Removing the other blocks releases all their memory.
    let highest = *queue
        .by_height
        .keys()
        .next_back()
        .expect("blocks are queued");
    queue.prune_by_height(highest);
    assert_eq!(queue.queued_bytes, 0);
    assert_eq!(queue.known_utxos.len(), 0);
    assert!(queue.blocks.is_empty());
    assert!(queue.by_parent.is_empty());
    assert!(queue.by_height.is_empty());

    Ok(())
}

/// Draining releases indexes and memory charges even if response receivers were cancelled, and
/// the empty queue can admit and dequeue the same blocks again.
#[test]
fn drain_releases_accounting_and_allows_requeue() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let child = block.make_fake_child();
    let mut queue = QueuedBlocks::default();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    queue.queue((block.clone().prepare(), cancelled_tx));
    drop(cancelled_rx);
    let (response_tx, mut response_rx) = oneshot::channel();
    queue.queue((child.clone().prepare(), response_tx));
    assert!(queue.queued_bytes > 0);

    let drained = queue.drain().collect::<Vec<_>>();
    assert_eq!(drained.len(), 2);
    assert!(queue.blocks.is_empty());
    assert!(queue.by_parent.is_empty());
    assert!(queue.by_height.is_empty());
    assert_eq!(queue.known_utxos.len(), 0);
    assert_eq!(queue.queued_bytes, 0);
    for (_, (_, response)) in drained {
        let _ = response.send(Err(CommitBlockError::OrphanQueueFull.into()));
    }
    assert_eq!(
        response_rx.try_recv(),
        Ok(Err(CommitBlockError::OrphanQueueFull.into()))
    );

    queue.queue(child.clone().into_queued());
    queue.prune_overflow();
    assert_eq!(queue.blocks.len(), 1);
    let dequeued = queue.dequeue_children(block.hash());
    assert_eq!(dequeued.len(), 1);
    assert_eq!(queue.queued_bytes, 0);
    assert_eq!(queue.known_utxos.len(), 0);
    assert!(queue.by_parent.is_empty());
    assert!(queue.by_height.is_empty());
    assert_eq!(CommitBlockError::OrphanQueueFull.misbehavior_score(), 0);
    Ok(())
}

/// A full queue fails abandoned leaves before live higher blocks, so a peer can't pin the queue
/// with low blocks whose callers stopped waiting.
#[test]
fn full_queue_fails_abandoned_leaves_first() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
    let mut queue = QueuedBlocks::default();

    // Low leaves with made-up parents, whose callers have stopped waiting.
    for i in 0..MAX_QUEUED_BLOCKS {
        block = block.make_fake_child();
        let mut leaf = Block::clone(&block);
        let mut parent = [0xA5; 32];
        parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
        Arc::make_mut(&mut leaf.header).previous_block_hash = block::Hash(parent);
        let (rsp_tx, rsp_rx) = oneshot::channel();
        queue.queue((Arc::new(leaf).prepare(), rsp_tx));
        drop(rsp_rx);
    }

    // A higher block that a caller is still waiting for.
    let (live_tx, mut live_rx) = oneshot::channel();
    queue.queue((block.make_fake_child().prepare(), live_tx));

    queue.prune_overflow();

    assert_eq!(queue.blocks.len(), MAX_QUEUED_BLOCKS);
    assert_eq!(
        live_rx.try_recv(),
        Err(TryRecvError::Empty),
        "the live block must outlive abandoned leaves"
    );

    Ok(())
}
