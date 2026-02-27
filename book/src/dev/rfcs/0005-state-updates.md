# State Updates

- Feature Name: state_updates
- Start Date: 2020-08-14
- Design PR: <https://github.com/ZcashFoundation/zebra/pull/902>
- Zebra Issue: <https://github.com/ZcashFoundation/zebra/issues/1049>

# Summary

[summary]: #summary

Zebra manages chain state in the `zebra-state` crate, which allows state
queries via asynchronous RPC (in the form of a Tower service). The state
system is responsible for contextual verification in the sense of [RFC2],
checking that new blocks are consistent with the existing chain state before
committing them. This RFC describes how the state is represented internally,
and how state updates are performed.

[RFC2]: ./0002-parallel-verification.md

# Motivation

[motivation]: #motivation

We need to be able to access and modify the chain state, and we want to have
a description of how this happens and what guarantees are provided by the
state service.

# Definitions

[definitions]: #definitions

- **state data**: Any data the state service uses to represent chain state.

- **structural/semantic/contextual verification**: as defined in [RFC2].

- **block chain**: A sequence of valid blocks linked by inclusion of the
  previous block hash in the subsequent block. Chains are rooted at the
  genesis block and extend to a **tip**.

- **chain state**: The state of the ledger after application of a particular
  sequence of blocks (state transitions).

- **block work**: The approximate amount of work required for a miner to generate
  a block hash that passes the difficulty filter. The number of block header
  attempts and the mining time are proportional to the work value. Numerically
  higher work values represent longer processing times.

- **cumulative work**: The sum of the **block work** of all blocks in a chain, from
  genesis to the chain tip.

- **best chain**: The chain with the greatest **cumulative work**. This chain
  represents the consensus state of the Zcash network and transactions.

- **side chain**: A chain which is not contained in the best chain.
  Side chains are pruned at the reorg limit, when they are no longer
  connected to the finalized state.

- **chain reorganization**: Occurs when a new best chain is found and the
  previous best chain becomes a side chain.

- **reorg limit**: The longest reorganization accepted by `zcashd`, 100 blocks.

- **orphaned block**: A block which is no longer included in the best chain.

- **non-finalized state**: State data corresponding to blocks above the reorg
  limit. This data can change in the event of a chain reorg.

- **finalized state**: State data corresponding to blocks below the reorg
  limit. This data cannot change in the event of a chain reorg.

- **non-finalized tips**: The highest blocks in each non-finalized chain. These
  tips might be at different heights.

- **finalized tip**: The highest block in the finalized state. The tip of the best
  chain is usually 100 blocks (the reorg limit) above the finalized tip. But it can
  be lower during the initial sync, and after a chain reorganization, if the new
  best chain is at a lower height.

- **relevant chain**: The relevant chain for a block starts at the previous
  block, and extends back to genesis.

- **relevant tip**: The tip of the relevant chain.

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

The `zebra-state` crate provides an implementation of the chain state storage
logic in a Zcash consensus node. Its main responsibility is to store chain
state, validating new blocks against the existing chain state in the process,
and to allow later querying of said chain state. `zebra-state` provides this
interface via a `tower::Service` based on the actor model with a
request/response interface for passing messages back and forth between the
state service and the rest of the application.

The main entry point for the `zebra-state` crate is the `init` function. This
function takes a `zebra_state::Config` and constructs a new state service,
which it returns wrapped by a `tower::Buffer`. This service is then interacted
with via the `tower::Service` trait.

```rust
use tower::{Service, ServiceExt};

let state = zebra_state::on_disk::init(state_config, network);
let request = zebra_state::Request::BlockLocator;
let response = state.ready_and().await?.call(request).await?;

assert!(matches!(response, zebra_state::Response::BlockLocator(_)));
```

**Note**: The `tower::Service` API requires that `ready` is always called
exactly once before each `call`. It is up to users of the zebra state service
to uphold this contract.

The `tower::Buffer` wrapper is `Clone`able, allowing shared access to a common state service. This allows different tasks to share access to the chain state.

The set of operations supported by `zebra-state` are encoded in its `Request`
enum. This enum has one variant for each supported operation.

```rust
pub enum Request {
    CommitBlock {
        block: Arc<Block>,
    },
    CommitFinalizedBlock {
        block: Arc<Block>,
    },
    Depth(Hash),
    Tip,
    BlockLocator,
    Transaction(Hash),
    Block(HashOrHeight),

    // .. some variants omitted
}
```

`zebra-state` breaks down its requests into two categories and provides
different guarantees for each category: requests that modify the state, and
requests that do not. Requests that update the state are guaranteed to run
sequentially and will never race against each other. Requests that read state
are done asynchronously and are guaranteed to read at least the state present
at the time the request was processed by the service, or a later state
present at the time the request future is executed. The state service avoids
race conditions between the read state and the written state by doing all
contextual verification internally.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

## State Components

Zcash (as implemented by `zcashd`) differs from Bitcoin in its treatment of
transaction finality. If a new best chain is detected that does not extend
the previous best chain, blocks at the end of the previous best chain become
orphaned (no longer included in the best chain). Their state updates are
therefore no longer included in the best chain's chain state. The process of
rolling back orphaned blocks and applying new blocks is called a chain
reorganization. Bitcoin allows chain reorganizations of arbitrary depth,
while `zcashd` limits chain reorganizations to 100 blocks. (In `zcashd`, the
new best chain must be a side-chain that forked within 100 blocks of the tip
of the current best chain.)

This difference means that in Bitcoin, chain state only has probabilistic
finality, while in Zcash, chain state is final once it is beyond the reorg
limit. To simplify our implementation, we split the representation of the
state data at the finality boundary provided by the reorg limit.

State data from blocks _above_ the reorg limit (_non-finalized state_) is
stored in-memory and handles multiple chains. State data from blocks _below_
the reorg limit (_finalized state_) is stored persistently using `rocksdb` and
only tracks a single chain. This allows a simplification of our state
handling, because only finalized data is persistent and the logic for
finalized data handles less invariants.

One downside of this design is that restarting the node loses the last 100
blocks, but node restarts are relatively infrequent and a short re-sync is
cheap relative to the cost of additional implementation complexity.

Another downside of this design is that we do not achieve exactly the same
behavior as `zcashd` in the event of a 51% attack: `zcashd` limits _each_ chain
reorganization to 100 blocks, but permits multiple reorgs, while Zebra limits
_all_ chain reorgs to 100 blocks. In the event of a successful 51% attack on
Zcash, this could be resolved by wiping the rocksdb state and re-syncing the new
chain, but in this scenario there are worse problems.

## Service Interface

[service-interface]: #service-interface

The state is accessed asynchronously through a Tower service interface.
Determining what guarantees the state service can and should provide to the
rest of the application requires considering two sets of behaviors:

1. behaviors related to the state's external API (a `Buffer`ed `tower::Service`);
2. behaviors related to the state's internal implementation (using `rocksdb`).

Making this distinction helps us to ensure we don't accidentally leak
"internal" behaviors into "external" behaviors, which would violate
encapsulation and make it more difficult to replace `rocksdb`.

In the first category, our state is presented to the rest of the application
as a `Buffer`ed `tower::Service`. The `Buffer` wrapper allows shared access
to a service using an actor model, moving the service to be shared into a
worker task and passing messages to it over an multi-producer single-consumer
(mpsc) channel. The worker task receives messages and makes `Service::call`s.
The `Service::call` method returns a `Future`, and the service is allowed to
decide how much work it wants to do synchronously (in `call`) and how much
work it wants to do asynchronously (in the `Future` it returns).

This means that our external API ensures that the state service sees a
linearized sequence of state requests, although the exact ordering is
unpredictable when there are multiple senders making requests.

Because the state service has exclusive access to the rocksdb database, and the
state service sees a linearized sequence of state requests, we have an easy
way to opt in to asynchronous database access. We can perform rocksdb operations
synchronously in the `Service::call`, waiting for them to complete, and be
sure that all future requests will see the resulting rocksdb state. Or, we can
perform rocksdb operations asynchronously in the future returned by
`Service::call`.

If we perform all _writes_ synchronously and allow reads to be either
synchronous or asynchronous, we ensure that writes cannot race each other.
Asynchronous reads are guaranteed to read at least the state present at the
time the request was processed, or a later state.

### Summary

- **rocksdb reads** may be done synchronously (in `call`) or asynchronously (in
  the `Future`), depending on the context;

- **rocksdb writes** must be done synchronously (in `call`)

## In-memory data structures

[in-memory]: #in-memory

At a high level, the in-memory data structures store a collection of chains,
each rooted at the highest finalized block. Each chain consists of a map from
heights to blocks. Chains are stored using an ordered map from cumulative work to
chains, so that the map ordering is the ordering of worst to best chains.

### The `Chain` type

[chain-type]: #chain-type

The `Chain` type represents a chain of blocks. Each block represents an
incremental state update, and the `Chain` type caches the cumulative state
update from its root to its tip.

The `Chain` type is used to represent the non-finalized portion of a complete
chain of blocks rooted at the genesis block. The parent block of the root of
a `Chain` is the tip of the finalized portion of the chain. As an exception, the finalized
portion of the chain is initially empty, until the genesis block has been finalized.

The `Chain` type supports several operations to manipulate chains, `push`,
`pop_root`, and `fork`. `push` is the most fundamental operation and handles
contextual validation of chains as they are extended. `pop_root` is provided
for finalization, and is how we move blocks from the non-finalized portion of
the state to the finalized portion. `fork` on the other hand handles creating
new chains for `push` when new blocks arrive whose parent isn't a tip of an
existing chain.

**Note:** The `Chain` type's API is only designed to handle non-finalized
data. The genesis block and all pre canopy blocks are always considered to
be finalized blocks and should not be handled via the `Chain` type through
`CommitBlock`. They should instead be committed directly to the finalized
state with `CommitFinalizedBlock`. This is particularly important with the
genesis block since the `Chain` will panic if used while the finalized state
is completely empty.

The `Chain` type is defined by the following struct and API:

```rust
#[derive(Debug, Clone)]
pub struct Chain {
    // The function `eq_internal_state` must be updated every time a field is added to [`Chain`].
    /// The configured network for this chain.
    network: Network,

    /// The contextually valid blocks which form this non-finalized partial chain, in height order.
    pub(crate) blocks: BTreeMap<block::Height, ContextuallyValidBlock>,

    /// An index of block heights for each block hash in `blocks`.
    pub height_by_hash: HashMap<block::Hash, block::Height>,

    /// An index of [`TransactionLocation`]s for each transaction hash in `blocks`.
    pub tx_loc_by_hash: HashMap<transaction::Hash, TransactionLocation>,

    /// The [`transparent::Utxo`]s created by `blocks`.
    ///
    /// Note that these UTXOs may not be unspent.
    /// Outputs can be spent by later transactions or blocks in the chain.
    //
    // TODO: replace OutPoint with OutputLocation?
    pub(crate) created_utxos: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// The [`transparent::OutPoint`]s spent by `blocks`,
    /// including those created by earlier transactions or blocks in the chain.
    pub(crate) spent_utxos: HashSet<transparent::OutPoint>,

    /// The Sprout note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    pub(super) sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
    /// The Sprout note commitment tree for each anchor.
    /// This is required for interstitial states.
    pub(crate) sprout_trees_by_anchor:
        HashMap<sprout::tree::Root, sprout::tree::NoteCommitmentTree>,
    /// The Sapling note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    pub(super) sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
    /// The Sapling note commitment tree for each height.
    pub(crate) sapling_trees_by_height: BTreeMap<block::Height, sapling::tree::NoteCommitmentTree>,
    /// The Orchard note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    pub(super) orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
    /// The Orchard note commitment tree for each height.
    pub(crate) orchard_trees_by_height: BTreeMap<block::Height, orchard::tree::NoteCommitmentTree>,
    /// The ZIP-221 history tree of the tip of this [`Chain`],
    /// including all finalized blocks, and the non-finalized `blocks` in this chain.
    pub(crate) history_tree: HistoryTree,

    /// The Sprout anchors created by `blocks`.
    pub(crate) sprout_anchors: MultiSet<sprout::tree::Root>,
    /// The Sprout anchors created by each block in `blocks`.
    pub(crate) sprout_anchors_by_height: BTreeMap<block::Height, sprout::tree::Root>,
    /// The Sapling anchors created by `blocks`.
    pub(crate) sapling_anchors: MultiSet<sapling::tree::Root>,
    /// The Sapling anchors created by each block in `blocks`.
    pub(crate) sapling_anchors_by_height: BTreeMap<block::Height, sapling::tree::Root>,
    /// The Orchard anchors created by `blocks`.
    pub(crate) orchard_anchors: MultiSet<orchard::tree::Root>,
    /// The Orchard anchors created by each block in `blocks`.
    pub(crate) orchard_anchors_by_height: BTreeMap<block::Height, orchard::tree::Root>,

    /// The Sprout nullifiers revealed by `blocks`.
    pub(super) sprout_nullifiers: HashSet<sprout::Nullifier>,
    /// The Sapling nullifiers revealed by `blocks`.
    pub(super) sapling_nullifiers: HashSet<sapling::Nullifier>,
    /// The Orchard nullifiers revealed by `blocks`.
    pub(super) orchard_nullifiers: HashSet<orchard::Nullifier>,

    /// Partial transparent address index data from `blocks`.
    pub(super) partial_transparent_transfers: HashMap<transparent::Address, TransparentTransfers>,

    /// The cumulative work represented by `blocks`.
    ///
    /// Since the best chain is determined by the largest cumulative work,
    /// the work represented by finalized blocks can be ignored,
    /// because they are common to all non-finalized chains.
    pub(super) partial_cumulative_work: PartialCumulativeWork,

    /// The chain value pool balances of the tip of this [`Chain`],
    /// including the block value pool changes from all finalized blocks,
    /// and the non-finalized blocks in this chain.
    ///
    /// When a new chain is created from the finalized tip,
    /// it is initialized with the finalized tip chain value pool balances.
    pub(crate) chain_value_pools: ValueBalance<NonNegative>,
}
```

#### `pub fn push(&mut self, block: Arc<Block>)`

Push a block into a chain as the new tip

1. Update cumulative data members
   - Add the block's hash to `height_by_hash`
   - Add work to `self.partial_cumulative_work`
   - For each `transaction` in `block`
     - Add key: `transaction.hash` and value: `(height, tx_index)` to `tx_loc_by_hash`
     - Add created utxos to `self.created_utxos`
     - Add spent utxos to `self.spent_utxos`
     - Add nullifiers to the appropriate `self.<version>_nullifiers`

2. Add block to `self.blocks`

#### `pub fn pop_root(&mut self) -> Arc<Block>`

Remove the lowest height block of the non-finalized portion of a chain.

1. Remove the lowest height block from `self.blocks`

2. Update cumulative data members
   - Remove the block's hash from `self.height_by_hash`
   - Subtract work from `self.partial_cumulative_work`
   - For each `transaction` in `block`
     - Remove `transaction.hash` from `tx_loc_by_hash`
     - Remove created utxos from `self.created_utxos`
     - Remove spent utxos from `self.spent_utxos`
     - Remove the nullifiers from the appropriate `self.<version>_nullifiers`

3. Return the block

#### `pub fn fork(&self, new_tip: block::Hash) -> Option<Self>`

Fork a chain at the block with the given hash, if it is part of this chain.

1. If `self` does not contain `new_tip` return `None`

2. Clone self as `forked`

3. While the tip of `forked` is not equal to `new_tip`
   - call `forked.pop_tip()` and discard the old tip

4. Return `forked`

#### `fn pop_tip(&mut self)`

Remove the highest height block of the non-finalized portion of a chain.

1. Remove the highest height `block` from `self.blocks`

2. Update cumulative data members
   - Remove the corresponding hash from `self.height_by_hash`
   - Subtract work from `self.partial_cumulative_work`
   - for each `transaction` in `block`
     - remove `transaction.hash` from `tx_loc_by_hash`
     - Remove created utxos from `self.created_utxos`
     - Remove spent utxos from `self.spent_utxos`
     - Remove the nullifiers from the appropriate `self.<version>_nullifiers`

#### `Ord`

The `Chain` type implements `Ord` for reorganizing chains. First chains are
compared by their `partial_cumulative_work`. Ties are then broken by
comparing `block::Hash`es of the tips of each chain. (This tie-breaker means
that all `Chain`s in the `NonFinalizedState` must have at least one block.)

**Note**: Unlike `zcashd`, Zebra does not use block arrival times as a
tie-breaker for the best tip. Since Zebra downloads blocks in parallel,
download times are not guaranteed to be unique. Using the `block::Hash`
provides a consistent tip order. (As a side-effect, the tip order is also
consistent after a node restart, and between nodes.)

#### `Default`

The `Chain` type implements `Default` for constructing new chains whose
parent block is the tip of the finalized state. This implementation should be
handled by `#[derive(Default)]`.

1. initialise cumulative data members
   - Construct an empty `self.blocks`, `height_by_hash`, `tx_loc_by_hash`,
     `self.created_utxos`, `self.spent_utxos`, `self.<version>_anchors`,
     `self.<version>_nullifiers`
   - Zero `self.partial_cumulative_work`

**Note:** The `ChainState` can be empty after a restart, because the
non-finalized state is empty.

### `NonFinalizedState` Type

[nonfinalizedstate-type]: #nonfinalizedstate-type

The `NonFinalizedState` type represents the set of all non-finalized state.
It consists of a set of non-finalized but verified chains and a set of
unverified blocks which are waiting for the full context needed to verify
them to become available.

`NonFinalizedState` is defined by the following structure and API:

```rust
/// The state of the chains in memory, including queued blocks.
#[derive(Debug, Default)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains.
    chain_set: BTreeSet<Chain>,
    /// Blocks awaiting their parent blocks for contextual verification.
    contextual_queue: QueuedBlocks,
}
```

#### `pub fn finalize(&mut self) -> Arc<Block>`

Finalize the lowest height block in the non-finalized portion of the best
chain and updates all side chains to match.

1. Extract the best chain from `self.chain_set` into `best_chain`

2. Extract the rest of the chains into a `side_chains` temporary variable, so
   they can be mutated

3. Remove the lowest height block from the best chain with
   `let finalized_block = best_chain.pop_root();`

4. Add `best_chain` back to `self.chain_set` if `best_chain` is not empty

5. For each remaining `chain` in `side_chains`
   - remove the lowest height block from `chain`
   - If that block is equal to `finalized_block` and `chain` is not empty add `chain` back to `self.chain_set`
   - Else, drop `chain`

6. Return `finalized_block`

#### `fn commit_block(&mut self, block: Arc<Block>)`

Commit `block` to the non-finalized state.

1. If the block is a pre-Canopy block, or the canopy activation block, panic.

2. If any chains tip hash equal `block.header.previous_block_hash` remove that chain from `self.chain_set`

3. Else Find the first chain that contains `block.parent` and fork it with
   `block.parent` as the new tip
   - `let fork = self.chain_set.iter().find_map(|chain| chain.fork(block.parent));`

4. Else panic, this should be unreachable because `commit_block` is only
   called when `block` is ready to be committed.

5. Push `block` into `parent_chain`

6. Insert `parent_chain` into `self.chain_set`

#### `pub(super) fn commit_new_chain(&mut self, block: Arc<Block>)`

Construct a new chain starting with `block`.

1. Construct a new empty chain

2. `push` `block` into that new chain

3. Insert the new chain into `self.chain_set`

### The `QueuedBlocks` type

The queued blocks type represents the non-finalized blocks that were committed
before their parent blocks were. It is responsible for tracking which blocks
are queued by their parent so they can be committed immediately after the
parent is committed. It also tracks blocks by their height so they can be
discarded if they ever end up below the reorg limit.

`NonFinalizedState` is defined by the following structure and API:

```rust
/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Debug, Default)]
struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedBlock>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, Vec<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, Vec<block::Hash>>,
}
```

#### `pub fn queue(&mut self, new: QueuedBlock)`

Add a block to the queue of blocks waiting for their requisite context to
become available.

1. extract the `parent_hash`, `new_hash`, and `new_height` from `new.block`

2. Add `new` to `self.blocks` using `new_hash` as the key

3. Add `new_hash` to the set of hashes in
   `self.by_parent.entry(parent_hash).or_default()`

4. Add `new_hash` to the set of hashes in
   `self.by_height.entry(new_height).or_default()`

#### `pub fn dequeue_children(&mut self, parent: block::Hash) -> Vec<QueuedBlock>`

Dequeue the set of blocks waiting on `parent`.

1. Remove the set of hashes waiting on `parent` from `self.by_parent`

2. Remove and collect each block in that set of hashes from `self.blocks` as
   `queued_children`

3. For each `block` in `queued_children` remove the associated `block.hash`
   from `self.by_height`

4. Return `queued_children`

#### `pub fn prune_by_height(&mut self, finalized_height: block::Height)`

Prune all queued blocks whose height are less than or equal to
`finalized_height`.

1. Split the `by_height` list at the finalized height, removing all heights
   that are below `finalized_height`

2. for each hash in the removed values of `by_height`
   - remove the corresponding block from `self.blocks`
   - remove the block's hash from the list of blocks waiting on
     `block.header.previous_block_hash` from `self.by_parent`

### Summary

- `Chain` represents the non-finalized portion of a single chain

- `NonFinalizedState` represents the non-finalized portion of all chains

- `QueuedBlocks` represents all unverified blocks that are waiting for
  context to be available.

The state service uses the following entry points:

- `commit_block` when it receives new blocks.

- `finalize` to prevent chains in `NonFinalizedState` from growing beyond the reorg limit.

- [FinalizedState.queue_and_commit_finalized_blocks](#committing-finalized-blocks) on the blocks returned by `finalize`, to commit those finalized blocks to disk.

## Committing non-finalized blocks

New `non-finalized` blocks are committed as follows:

### `pub(super) fn queue_and_commit_non_finalized_blocks(&mut self, new: Arc<Block>) -> tokio::sync::oneshot::Receiver<block::Hash>`

1. If a duplicate block hash exists in a non-finalized chain, or the finalized chain,
   it has already been successfully verified:
   - create a new oneshot channel
   - immediately send `Err(DuplicateBlockHash)` drop the sender
   - return the receiver

2. If a duplicate block hash exists in the queue:
   - Find the `QueuedBlock` for that existing duplicate block
   - create a new channel for the new request
   - replace the old sender in `queued_block` with the new sender
   - send `Err(DuplicateBlockHash)` through the old sender channel
   - continue to use the new receiver

3. Else create a `QueuedBlock` for `block`:
   - Create a `tokio::sync::oneshot` channel
   - Use that channel to create a `QueuedBlock` for `block`
   - Add `block` to `self.queued_blocks`
   - continue to use the new receiver

4. If `block.header.previous_block_hash` is not present in the finalized or
   non-finalized state:
   - Return the receiver for the block's channel

5. Else iteratively attempt to process queued blocks by their parent hash
   starting with `block.header.previous_block_hash`

6. While there are recently committed parent hashes to process
   - Dequeue all blocks waiting on `parent` with `let queued_children =
self.queued_blocks.dequeue_children(parent);`
   - for each queued `block`
     - **Run contextual validation** on `block`
       - contextual validation should check that the block height is
         equal to the previous block height plus 1. This check will
         reject blocks with invalid heights.
     - If the block fails contextual validation send the result to the
       associated channel
     - Else if the block's previous hash is the finalized tip add to the
       non-finalized state with `self.mem.commit_new_chain(block)`
     - Else add the new block to an existing non-finalized chain or new fork
       with `self.mem.commit_block(block);`
     - Send `Ok(hash)` over the associated channel to indicate the block
       was successfully committed
     - Add `block.hash` to the set of recently committed parent hashes to
       process

7. While the length of the non-finalized portion of the best chain is greater
   than the reorg limit
   - Remove the lowest height block from the non-finalized state with
     `self.mem.finalize();`
   - Commit that block to the finalized state with
     `self.disk.commit_finalized_direct(finalized);`

8. Prune orphaned blocks from `self.queued_blocks` with
   `self.queued_blocks.prune_by_height(finalized_height);`

9. Return the receiver for the block's channel

## rocksdb data structures

[rocksdb]: #rocksdb

The current database format is documented in [Upgrading the State Database](../state-db-upgrades.md).

## Committing finalized blocks

If the parent block is not committed, add the block to an internal queue for
future processing. Otherwise, commit the block described below, then
commit any queued children. (Although the checkpointer generates verified
blocks in order when it completes a checkpoint, the blocks are committed in the
response futures, so they may arrive out of order).

Committing a block to the rocksdb state should be implemented as a wrapper around
a function also called by [`Request::CommitBlock`](#request-commit-block),
which should:

#### `pub(super) fn queue_and_commit_finalized_blocks(&mut self, queued_block: QueuedBlock)`

1. Obtain the highest entry of `hash_by_height` as `(old_height, old_tip)`.
   Check that `block`'s parent hash is `old_tip` and its height is
   `old_height+1`, or panic. This check is performed as defense-in-depth to
   prevent database corruption, but it is the caller's responsibility (e.g. the
   zebra-state service's responsibility) to commit finalized blocks in order.

The genesis block does not have a parent block. For genesis blocks,
check that `block`'s parent hash is `null` (all zeroes) and its height is `0`.

1. Insert the block and transaction data into the relevant column families.

2. If the block is a genesis block, skip any transaction updates.

   (Due to a [bug in zcashd](https://github.com/ZcashFoundation/zebra/issues/559),
   genesis block anchors and transactions are ignored during validation.)

3. Update the block anchors, history tree, and chain value pools.

4. Iterate over the enumerated transactions in the block. For each transaction,
   update the relevant column families.

**Note**: The Sprout and Sapling anchors are the roots of the Sprout and
Sapling note commitment trees that have already been calculated for the last
transaction(s) in the block that have `JoinSplit`s in the Sprout case and/or
`Spend`/`Output` descriptions in the Sapling case. These should be passed as
fields in the `Commit*Block` requests.

Due to the coinbase maturity rules, the Sprout root is the empty root
for the first 100 blocks. (These rules are already implemented in contextual
validation and the anchor calculations.)

Hypothetically, if Sapling were activated from genesis, the specification requires
a Sapling anchor, but `zcashd` would ignore that anchor.

[`JoinSplit`]: https://zebra.zfnd.org/internal/zebra_chain/sprout/struct.JoinSplit.html
[`Spend`]: https://zebra.zfnd.org/internal/zebra_chain/sapling/spend/struct.Spend.html
[`Action`]: https://zebra.zfnd.org/internal/zebra_chain/orchard/struct.Action.html

These updates can be performed in a batch or without necessarily iterating
over all transactions, if the data is available by other means; they're
specified this way for clarity.

## Accessing previous blocks for contextual validation

[previous-block-context]: #previous-block-context

The state service performs contextual validation of blocks received via the
`CommitBlock` request. Since `CommitBlock` is synchronous, contextual validation
must also be performed synchronously.

The relevant chain for a block starts at its previous block, and follows the
chain of previous blocks back to the genesis block.

### Relevant chain iterator

[relevant-chain-iterator]: #relevant-chain-iterator

The relevant chain can be retrieved from the state service as follows:

- if the previous block is the finalized tip:
  - get recent blocks from the finalized state
- if the previous block is in the non-finalized state:
  - get recent blocks from the relevant chain, then
  - get recent blocks from the finalized state, if required

The relevant chain can start at any non-finalized block, or at the finalized tip.

### Relevant chain implementation

[relevant-chain-implementation]: #relevant-chain-implementation

The relevant chain is implemented as a `StateService` iterator, which returns
`Arc<Block>`s.

The chain iterator implements `ExactSizeIterator`, so Zebra can efficiently
assert that the relevant chain contains enough blocks to perform each contextual
validation check.

```rust
impl StateService {
    /// Return an iterator over the relevant chain of the block identified by
    /// `hash`.
    ///
    /// The block identified by `hash` is included in the chain of blocks yielded
    /// by the iterator.
    pub fn chain(&self, hash: block::Hash) -> Iter<'_> { ... }
}

impl Iterator for Iter<'_>  {
    type Item = Arc<Block>;
    ...
}
impl ExactSizeIterator for Iter<'_> { ... }
impl FusedIterator for Iter<'_> {}
```

For further details, see [PR 1271].

[PR 1271]: https://github.com/ZcashFoundation/zebra/pull/1271

## Request / Response API

[request-response]: #request-response

The state API is provided by a pair of `Request`/`Response` enums. Each
`Request` variant corresponds to particular `Response` variants, and it's
fine (and encouraged) for caller code to unwrap the expected variants with
`unreachable!` on the unexpected variants. This is slightly inconvenient but
it means that we have a unified state interface with unified backpressure.

This API includes both write and read calls. Spotting `Commit` requests in
code review should not be a problem, but in the future, if we need to
restrict access to write calls, we could implement a wrapper service that
rejects these, and export "read" and "write" frontends to the same inner service.

### `Request::CommitBlock`

[request-commit-block]: #request-commit-block

```rust
CommitBlock {
    block: Arc<Block>,
    sprout_anchor: sprout::tree::Root,
    sapling_anchor: sapling::tree::Root,
}
```

Performs contextual validation of the given block, committing it to the state
if successful. Returns `Response::Added(block::Hash)` with the hash of
the newly committed block or an error.

### `Request::CommitFinalizedBlock`

[request-commit-finalized-block]: #request-finalized-block

```rust
CommitFinalizedBlock {
    block: Arc<Block>,
    sprout_anchor: sprout::tree::Root,
    sapling_anchor: sapling::tree::Root,
}
```

Commits a finalized block to the rocksdb state, skipping contextual validation.
This is exposed for use in checkpointing, which produces in-order finalized
blocks. Returns `Response::Added(block::Hash)` with the hash of the
committed block if successful.

### `Request::Depth(block::Hash)`

[request-depth]: #request-depth

Computes the depth in the best chain of the block identified by the given
hash, returning

- `Response::Depth(Some(depth))` if the block is in the best chain;
- `Response::Depth(None)` otherwise.

Implemented by querying:

- (non-finalized) the `height_by_hash` map in the best chain, and
- (finalized) the `height_by_hash` tree

### `Request::Tip`

[request-tip]: #request-tip

Returns `Response::Tip(block::Hash)` with the current best chain tip.

Implemented by querying:

- (non-finalized) the highest height block in the best chain
- (finalized) the highest height block in the `hash_by_height` tree, if the `non-finalized` state is empty

### `Request::BlockLocator`

[request-block-locator]: #request-block-locator

Returns `Response::BlockLocator(Vec<block::Hash>)` with hashes starting from
the current chain tip and reaching backwards towards the genesis block. The
first hash is the best chain tip. The last hash is the tip of the finalized
portion of the state. If the finalized and non-finalized states are both
empty, the block locator is also empty.

This can be used by the sync component to request hashes of subsequent
blocks.

Implemented by querying:

- (non-finalized) the `hash_by_height` map in the best chain
- (finalized) the `hash_by_height` tree.

### `Request::Transaction(transaction::Hash)`

[request-transaction]: #request-transaction

Returns

- `Response::Transaction(Some(Transaction))` if the transaction identified by
  the given hash is contained in the state;

- `Response::Transaction(None)` if the transaction identified by the given
  hash is not contained in the state.

Implemented by querying:

- (non-finalized) the `tx_loc_by_hash` map (to get the block that contains the
  transaction) of each chain starting with the best chain, and then find
  block that chain's `blocks` (to get the block containing the transaction
  data)
- (finalized) the `tx_loc_by_hash` tree (to get the block that contains the
  transaction) and then `block_header_by_height` tree (to get the block
  containing the transaction data), if the transaction is not in any
  non-finalized chain

### `Request::Block(block::Hash)`

[request-block]: #request-block

Returns

- `Response::Block(Some(Arc<Block>))` if the block identified by the given
  hash is contained in the state;

- `Response::Block(None)` if the block identified by the given hash is not
  contained in the state;

Implemented by querying:

- (non-finalized) the `height_by_hash` of each chain starting with the best
  chain, then find block that chain's `blocks` (to get the block data)
- (finalized) the `height_by_hash` tree (to get the block height) and then the
  `block_header_by_height` tree (to get the block data), if the block is not in
  any non-finalized chain

### `Request::AwaitSpendableUtxo { outpoint: OutPoint, spend_height: Height, spend_restriction: SpendRestriction }`

Returns

- `Response::SpendableUtxo(transparent::Output)`

Implemented by querying:

- (non-finalized) if any `Chains` contain `OutPoint` in their `created_utxos`,
  return the `Utxo` for `OutPoint`;
- (finalized) else if `OutPoint` is in `utxos_by_outpoint`,
  return the `Utxo` for `OutPoint`;
- else wait for `OutPoint` to be created as described in [RFC0004];

Then validating:

- check the transparent coinbase spend restrictions specified in [RFC0004];
- if the restrictions are satisfied, return the response;
- if the spend is invalid, drop the request (and the caller will time out).

[RFC0004]: https://zebra.zfnd.org/dev/rfcs/0004-asynchronous-script-verification.html

# Drawbacks

[drawbacks]: #drawbacks

- Restarts can cause `zebrad` to redownload up to the last one hundred blocks
  it verified in the best chain, and potentially some recent side-chain blocks.

- The service interface puts some extra responsibility on callers to ensure
  it is used correctly and does not verify the usage is correct at compile
  time.

- the service API is verbose and requires manually unwrapping enums

- We do not handle reorgs the same way `zcashd` does, and could in theory need
  to delete our entire on disk state and resync the chain in some
  pathological reorg cases.
- testnet rollbacks are infrequent, but possible, due to bugs in testnet
  releases. Each testnet rollback will require additional state service code.
