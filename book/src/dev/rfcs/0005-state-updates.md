# State Updates

- Feature Name: state_updates
- Start Date: 2020-08-14
- Design PR: https://github.com/ZcashFoundation/zebra/pull/902
- Zebra Issue: https://github.com/ZcashFoundation/zebra/issues/1049


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

* **state data**: Any data the state service uses to represent chain state.

* **structural/semantic/contextual verification**: as defined in [RFC2].

* **block chain**: A sequence of valid blocks linked by inclusion of the
  previous block hash in the subsequent block. Chains are rooted at the
  *genesis* block and extend to a *tip*.

* **chain state**: The state of the ledger after application of a particular
  sequence of blocks (state transitions).

* **difficulty**: The cumulative proof-of-work from genesis to the chain tip.

* **best chain**: The chain with the greatest difficulty. This chain
  represents the consensus state of the Zcash network and transactions.

* **side chain**: A chain which is not contained in the best chain.
  Side chains are pruned at the reorg limit, when they are no longer
  connected to the finalized state.

* **chain reorganization**: Occurs when a new best chain is found and the
  previous best chain becomes a side chain.

* **reorg limit**: The longest reorganization accepted by Zcashd, 100 blocks.

* **orphaned block**: A block which is no longer included in the best chain.

* **non-finalized state**: State data corresponding to blocks above the reorg
  limit. This data can change in the event of a chain reorg.

* **finalized state**: State data corresponding to blocks below the reorg
  limit. This data cannot change in the event of a chain reorg.

* **non-finalized tips**: The highest blocks in each non-finalized chain. These
  tips might be at different heights.

* **finalized tip**: The highest block in the finalized state. The tip of the best
  chain is usually 100 blocks (the reorg limit) above the finalized tip. But it can
  be lower during the initial sync, and after a chain reorganization, if the new
  best chain is at a lower height.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

The `zebra-state` crate provides an implementation of the chain state storage
logic in a zcash consensus node. Its main responsibility is to store chain
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

The `tower::Buffer` wrapper is `Clone`able, allowing shared access to a common state service.  This allows different tasks to share access to the chain state.

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
different guarantees for each category: requests that modify the state, and requests that
do not. Requests that update the state are guaranteed to run sequentially and
will never race against each other. Requests that read state are done
asynchronously and are guaranteed to read at least the state present at the
time the request was processed by the service, or a later state present at the time the request future is executed. The state service avoids
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

State data from blocks *above* the reorg limit (*non-finalized state*) is
stored in-memory and handles multiple chains. State data from blocks *below*
the reorg limit (*finalized state*) is stored persistently using `sled` and
only tracks a single chain. This allows a simplification of our state
handling, because only finalized data is persistent and the logic for
finalized data handles less invariants.

One downside of this design is that restarting the node loses the last 100
blocks, but node restarts are relatively infrequent and a short re-sync is
cheap relative to the cost of additional implementation complexity.

Another downside of this design is that we do not achieve exactly the same
behavior as Zcashd in the event of a 51% attack: Zcashd limits *each* chain
reorganization to 100 blocks, but permits multiple reorgs, while Zebra limits
*all* chain reorgs to 100 blocks. In the event of a successful 51% attack on
Zcash, this could be resolved by wiping the Sled state and re-syncing the new
chain, but in this scenario there are worse problems.

## Service Interface
[service-interface]: #service-interface

The state is accessed asynchronously through a Tower service interface.
Determining what guarantees the state service can and should provide to the
rest of the application requires considering two sets of behaviors:

1. behaviors related to the state's external API (a `Buffer`ed `tower::Service`);
2. behaviors related to the state's internal implementation (using `sled`).

Making this distinction helps us to ensure we don't accidentally leak
"internal" behaviors into "external" behaviors, which would violate
encapsulation and make it more difficult to replace `sled`.

In the first category, our state is presented to the rest of the application
as a `Buffer`ed `tower::Service`. The `Buffer` wrapper allows shared access
to a service using an actor model, moving the service to be shared into a
worker task and passing messages to it over an multi-producer single-consumer
(mpsc) channel. The worker task recieves messages and makes `Service::call`s.
The `Service::call` method returns a `Future`, and the service is allowed to
decide how much work it wants to do synchronously (in `call`) and how much
work it wants to do asynchronously (in the `Future` it returns).

This means that our external API ensures that the state service sees a
linearized sequence of state requests, although the exact ordering is
unpredictable when there are multiple senders making requests.

In the second category, the Sled API presents itself synchronously, but
database and tree handles are clonable and can be moved between threads. All
that's required to process some request asynchronously is to clone the
appropriate handle, move it into an async block, and make the call as part of
the future. (We might want to use Tokio's blocking API for this, but this is
an implementation detail).

Because the state service has exclusive access to the sled database, and the
state service sees a linearized sequence of state requests, we have an easy
way to opt in to asynchronous database access. We can perform sled operations
synchronously in the `Service::call`, waiting for them to complete, and be
sure that all future requests will see the resulting sled state. Or, we can
perform sled operations asynchronously in the future returned by
`Service::call`.

If we perform all *writes* synchronously and allow reads to be either
synchronous or asynchronous, we ensure that writes cannot race each other.
Asynchronous reads are guaranteed to read at least the state present at the
time the request was processed, or a later state.

### Summary

- **Sled reads** may be done synchronously (in `call`) or asynchronously (in
  the `Future`), depending on the context;

- **Sled writes** must be done synchronously (in `call`)

## In-memory data structures
[in-memory]: #in-memory

At a high level, the in-memory data structures store a collection of chains,
each rooted at the highest finalized block. Each chain consists of a map from
heights to blocks. Chains are stored using an ordered map from cumulative work to
chains, so that the map ordering is the ordering of best to worst chains.

### The `Chain` type
[chain-type]: #chain-type


The `Chain` type represents a chain of blocks. Each block represents an
incremental state update, and the `Chain` type caches the cumulative state
update from its root to its tip.

The `Chain` type is used to represent the non-finalized portion of a complete
chain of blocks rooted at the genesis block. The parent block of the root of
a `Chain` is the tip of the finalized portion of the chain. As an exception, the finalized
portion of the chain is initially empty, until the genesis block has been finalized.

The `Chain` type supports serveral operations to manipulate chains, `push`,
`pop_root`, and `fork`. `push` is the most fundamental operation and handles
contextual validation of chains as they are extended. `pop_root` is provided
for finalization, and is how we move blocks from the non-finalized portion of
the state to the finalized portion. `fork` on the other hand handles creating
new chains for `push` when new blocks arrive whose parent isn't a tip of an
existing chain.

**Note:** The `Chain` type's API is only designed to handle non-finalized
data. The genesis block and all pre sapling blocks are always considered to
be finalized blocks and should not be handled via the `Chain` type through
`CommitBlock`. They should instead be committed directly to the finalized
state with `CommitFinalizedBlock`. This is particularly important with the
genesis block since the `Chain` will panic if used while the finalized state
is completely empty.

The `Chain` type is defined by the following struct and API:

```rust
struct Chain {
    blocks: BTreeMap<block::Height, Arc<Block>>,
    height_by_hash: HashMap<block::Hash, block::Height>,
    tx_by_hash: HashMap<transaction::Hash, (block::Height, tx_index)>,

    utxos: HashSet<transparent::Output>,
    sapling_anchors: HashSet<sapling::tree::Root>,
    sprout_anchors: HashSet<sprout::tree::Root>,
    sapling_nullifiers: HashSet<sapling::Nullifier>,
    sprout_nullifiers: HashSet<sprout::Nullifier>,
    partial_cumulative_work: PartialCumulativeWork,
}
```

#### `pub fn push(&mut self, block: Arc<Block>)`

Push a block into a chain as the new tip

1. Update cumulative data members
    - Add block to end of `self.blocks`
    - Add hash to `height_by_hash`
    - for each `transaction` in `block`
      - add key: `transaction.hash` and value: `(height, tx_index)` to `tx_by_hash`
    - Add new utxos and remove consumed utxos from `self.utxos`
    - Add anchors to the appropriate `self.<version>_anchors`
    - Add nullifiers to the appropriate `self.<version>_nullifiers`
    - Add work to `self.partial_cumulative_work`

#### `pub fn pop_root(&mut self) -> Arc<Block>`

Remove the lowest height block of the non-finalized portion of a chain.

1. Remove the lowest height block from `self.blocks`

2. Update cumulative data members
    - Remove the block's hash from `self.height_by_hash`
    - for each `transaction` in `block`
      - remove `transaction.hash` from `tx_by_hash`
    - Remove new utxos from `self.utxos`
    - Remove the anchors from the appropriate `self.<version>_anchors`
    - Remove the nullifiers from the appropriate `self.<version>_nullifiers`

3. Return the block

#### `pub fn fork(&self, new_tip: block::Hash) -> Option<Self>`

Fork a chain at the block with the given hash, if it is part of this chain.

1. If `self` does not contain `new_tip` return `None`

2. Clone self as `forked`

3. While the tip of `forked` is not equal to `new_tip`
   - call `forked.pop_tip()` and discard the old tip

4. Return `forked`

#### `fn pop_tip(&mut self) -> Arc<Block>`

Remove the highest height block of the non-finalized portion of a chain.

1. Remove the highest height `block` from `self.blocks`

2. Update cumulative data members
    - Remove the corresponding hash from `self.height_by_hash`
    - for each `transaction` in `block`
      - remove `transaction.hash` from `tx_by_hash`
    - Add consumed utxos and remove new utxos from `self.utxos`
    - Remove anchors from the appropriate `self.<version>_anchors`
    - Remove the nullifiers from the appropriate `self.<version>_nullifiers`
    - Subtract work from `self.partial_cumulative_work`

3. Return the block

#### `Ord`

The `Chain` type implements `Ord` for reorganizing chains. First chains
are compared by their `partial_cumulative_work`. Ties are then broken by
comparing `block::Hash`es of the tips of each chain.

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
    - Construct an empty `self.blocks`, `height_by_hash`, `tx_by_hash`,
    `self.utxos`, `self.<version>_anchors`, `self.<version>_nullifiers`
    - Zero `self.partial_cumulative_work`

**Note:** The chain can be empty if:
  - after a restart - the non-finalized state is empty
  - during a fork from the finalized tip - the forked Chain is empty, because
    all its blocks have been `pop`ped


### `ChainSet` Type
[chainset-type]: #chainset-type

The `ChainSet` type represents the set of all non-finalized state. It
consists of a set of non-finalized but verified chains and a set of
unverified blocks which are waiting for the full context needed to verify
them to become available.

`ChainSet` is defined by the following structure and API:

```rust
struct ChainSet {
    chains: BTreeSet<Chain>,

    queued_blocks: BTreeMap<block::Hash, QueuedBlock>,
    queued_by_parent: BTreeMap<block::Hash, Vec<block::Hash>>,
    queued_by_height: BTreeMap<block::Height, Vec<block::Hash>>,
}
```

#### `pub fn finalize(&mut self) -> Arc<Block>`

Finalize the lowest height block in the non-finalized portion of the best
chain and updates all side chains to match.

1. Extract the best chain from `self.chains` into `best_chain`

2. Extract the rest of the chains into a `side_chains` temporary variable, so
   they can be mutated

3. Remove the lowest height block from the best chain with
   `let block = best_chain.pop_root();`

4. Add `best_chain` back to `self.chains`

5. For each remaining `chain` in `side_chains`
    - If `chain` starts with `block`, remove `block` and add `chain` back to
    `self.chains`
    - Else, drop `chain`

6. calculate the new finalized tip height from the new `best_chain`

7. for each `height` in `self.queued_by_height` where the height is lower than the
   new reorg limit
   - for each `hash` in `self.queued_by_height.remove(height)`
     - Remove the key `hash` from `self.queued_blocks` and store the removed `block`
     - Find and remove `hash` from `self.queued_by_parent` using `block.parent`'s hash

8. Return `block`

### `pub fn queue(&mut self, block: QueuedBlock)`

Queue a non-finalized block to be committed to the state.

After queueing a non-finalized block, this method checks whether the newly
queued block (and any of its descendants) can be committed to the state

1. Check if the parent block exists in any current chain

2. If it does, call `let ret = self.commit_block(block)`
    - Call `self.process_queued(new_parents)` if `ret` is `Some`

3. Else Add `block` to `self.queued_blocks` and related members and return

### `fn process_queued(&mut self, new_parent: block::Hash)`

1. Create a list of `new_parents` and populate it with `new_parent`

2. While let Some(parent) = new_parents.pop()
    - for each `hash` in `self.queued_by_parent.remove(&parent.hash)`
      - lookup the `block` for `hash`
      - remove `block` from `self.queued_blocks`
      - remove `hash` from `self.queued_by_height`
      - let result = `self.commit_block(block)`;
      - add `result` to `new_parents`

### `fn commit_block(&mut self, block: QueuedBlock) -> Option<block::Hash>`

Try to commit `block` to the non-finalized state. Returns `None` if the block
cannot be committed due to missing context.

1. Search for the first chain where `block.parent` == `chain.tip`. If it exists:
    - push `block` onto that chain
    - broadcast `result` via `block.rsp_tx`
    - return Some(block.hash) if `result.is_ok()`

2. Find the first chain that contains `block.parent` and fork it with
  `block.parent` as the new tip
    - `let fork = self.chains.iter().find_map(|chain| chain.fork(block.parent));`

3. If `fork` is `Some`
    - push `block` onto that chain
    - add `fork` to `self.chains`
    - broadcast `result` via `block.rsp_tx`
    - return Some(block.hash) if `result.is_ok()`

5. Else panic, this should be unreachable because `commit_block` is only
   called when it's ready to be committed.

### Summary

- `Chain` represents the non-finalized portion of a single chain

- `ChainSet` represents the non-finalized portion of all chains and all
  unverified blocks that are waiting for context to be available.

- `ChainSet::queue` handles queueing and or commiting blocks and
  reorganizing chains (via `commit_block`) but not finalizing them

- Finalized blocks are returned from `finalize` and must still be committed
  to disk afterwards

- `finalize` handles pruning queued blocks that are past the reorg limit

## Committing non-finalized blocks

Given the above structures for manipulating the non-finalized state new
`non-finalized` blocks are commited in two steps. First we commit the block
to the in memory state, then we finalize all lowest height blocks that are
past the reorg limit, finally we process any queued blocks and prune any that
are now past the reorg limit.

1. Run contextual validation on `block` against the finalized and non
   finalized state

2. If `block.parent` == `finalized_tip.hash`
    - Construct a new `Chain` with `Chain::default`
    - push `block` onto that chain
    - add `fork` to `chain_set.chains`
    - broadcast `result` via `block.rsp_tx`
    - return Some(block.hash) if `result.is_ok()`

3. commit or queue the block to the non-finalized state with
   `chain_set.queue(block);`

4. If the best chain is longer than the reorg limit
    - Finalize all lowest height blocks in the best chain, and commit them to
    disk with `CommitFinalizedBlock`:

      ```
      while self.best_chain().len() > reorg_limit {
        let finalized = chain_set.finalize()?;
        let request = CommitFinalizedBlock { finalized };
        sled_state.ready_and().await?.call(request).await?;
      };
      ```

## Sled data structures
[sled]: #sled

Sled provides a persistent, thread-safe `BTreeMap<&[u8], &[u8]>`. Each map is
a distinct "tree". Keys are sorted using lex order on byte strings, so
integer values should be stored using big-endian encoding (so that the lex
order on byte strings is the numeric ordering).

We use the following Sled trees:

| Tree                 |                  Keys |                              Values |
|----------------------|-----------------------|-------------------------------------|
| `hash_by_height`     | `BE32(height)`        | `block::Hash`                       |
| `height_by_hash`     | `block::Hash`         | `BE32(height)`                      |
| `block_by_height`    | `BE32(height)`        | `Block`                             |
| `tx_by_hash`         | `transaction::Hash`   | `BE32(height) || BE32(tx_index)`    |
| `utxo_by_outpoint`   | `OutPoint`            | `TransparentOutput`                 |
| `sprout_nullifiers`  | `sprout::Nullifier`   | `()`                                |
| `sapling_nullifiers` | `sapling::Nullifier`  | `()`                                |
| `sprout_anchors`     | `sprout::tree::Root`  | `()`                                |
| `sapling_anchors`    | `sapling::tree::Root` | `()`                                |

Zcash structures are encoded using `ZcashSerialize`/`ZcashDeserialize`.

**Note:** We do not store the cumulative work for the finalized chain, because the finalized work is equal for all non-finalized chains. So the additional non-finalized work can be used to calculate the relative chain order, and choose the best chain.

### Notes on Sled trees

- The `hash_by_height` and `height_by_hash` trees provide the bijection between
  block heights and block hashes.  (Since the Sled state only stores finalized
  state, this is actually a bijection).

- Blocks are stored by height, not by hash.  This has the downside that looking
  up a block by hash requires an extra level of indirection.  The upside is
  that blocks with adjacent heights are adjacent in the database, and many
  common access patterns, such as helping a client sync the chain or doing
  analysis, access blocks in (potentially sparse) height order.  In addition,
  the fact that we commit blocks in order means we're writing only to the end
  of the Sled tree, which may help save space.

- Transaction references are stored as a `(height, index)` pair referencing the
  height of the transaction's parent block and the transaction's index in that
  block.  This would more traditionally be a `(hash, index)` pair, but because
  we store blocks by height, storing the height saves one level of indirection.

## Committing finalized blocks

If the parent block is not committed, add the block to an internal queue for
future processing.  Otherwise, commit the block described below, then
commit any queued children.  (Although the checkpointer generates verified
blocks in order when it completes a checkpoint, the blocks are committed in the
response futures, so they may arrive out of order).

Committing a block to the sled state should be implemented as a wrapper around
a function also called by [`Request::CommitBlock`](#request-commit-block),
which should:

1. Obtain the highest entry of `hash_by_height` as `(old_height, old_tip)`.
Check that `block`'s parent hash is `old_tip` and its height is
`old_height+1`, or panic. This check is performed as defense-in-depth to
prevent database corruption, but it is the caller's responsibility (e.g. the
zebra-state service's responsibility) to commit finalized blocks in order.

The genesis block does not have a parent block. For genesis blocks,
check that `block`'s parent hash is `null` (all zeroes) and its height is `0`.

2. Insert:
    - `(hash, height)` into `height_by_hash`;
    - `(height, hash)` into `hash_by_height`;
    - `(height, block)` into `block_by_height`.

3. If the block is a genesis block, skip any transaction updates.

(Due to a [bug in zcashd](https://github.com/ZcashFoundation/zebra/issues/559), genesis block transactions
are ignored during validation.)

3. If the block is a genesis block, skip any transaction updates.

(Due to a [bug in zcashd](https://github.com/ZcashFoundation/zebra/issues/559), genesis block transactions
are ignored during validation.)

3.  Update the `sprout_anchors` and `sapling_anchors` trees with the Sprout and Sapling anchors.

4. Iterate over the enumerated transactions in the block. For each transaction:

   1. Insert `(transaction_hash, block_height || BE32(tx_index))` to
   `tx_by_hash`;

   2. For each `TransparentInput::PrevOut { outpoint, .. }` in the
   transaction's `inputs()`, remove `outpoint` from `utxo_by_output`.

   3. For each `output` in the transaction's `outputs()`, construct the
   `outpoint` that identifies it, and insert `(outpoint, output)` into
   `utxo_by_output`.

   4. For each [`JoinSplit`] description in the transaction,
   insert `(nullifiers[0],())` and `(nullifiers[1],())` into
   `sprout_nullifiers`.

   5. For each [`Spend`] description in the transaction, insert
   `(nullifier,())` into `sapling_nullifiers`.

**Note**: The Sprout and Sapling anchors are the roots of the Sprout and
Sapling note commitment trees that have already been calculated for the last
transaction(s) in the block that have `JoinSplit`s in the Sprout case and/or
`Spend`/`Output` descriptions in the Sapling case. These should be passed as
fields in the `Commit*Block` requests.

[`JoinSplit`]: https://doc.zebra.zfnd.org/zebra_chain/transaction/struct.JoinSplit.html
[`Spend`]: https://doc.zebra.zfnd.org/zebra_chain/transaction/struct.Spend.html

These updates can be performed in a batch or without necessarily iterating
over all transactions, if the data is available by other means; they're
specified this way for clarity.


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
if successful. Returns `Response::Added(BlockHeaderHash)` with the hash of
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

Commits a finalized block to the sled state, skipping contextual validation.
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
if the `non-finalized` state is empty
- (finalized) the highest height block in the `hash_by_height` tree

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

### `Request::Transaction(TransactionHash)`
[request-transaction]: #request-transaction

Returns

- `Response::Transaction(Some(Transaction))` if the transaction identified by
    the given hash is contained in the state;

- `Response::Transaction(None)` if the transaction identified by the given
    hash is not contained in the state.

Implemented by querying:

- (non-finalized) the `tx_by_hash` map (to get the block that contains the
  transaction) of each chain starting with the best chain, and then find
  block that chain's `blocks` (to get the block containing the transaction
  data)
if the transaction is not in any non-finalized chain:
- (finalized) the `tx_by_hash` tree (to get the block that contains the
  transaction) and then `block_by_height` tree (to get the block containing
  the transaction data).

### `Request::Block(BlockHeaderHash)`
[request-block]: #request-block

Returns

- `Response::Block(Some(Arc<Block>))` if the block identified by the given
    hash is contained in the state;

- `Response::Block(None)` if the block identified by the given hash is not
    contained in the state;

Implemented by querying:

- (non-finalized) the `height_by_hash` of each chain starting with the best
  chain, then find block that chain's `blocks` (to get the block data)
if the block is not in any non-finalized chain:
- (finalized) the `height_by_hash` tree (to get the block height) and then
    the `block_by_height` tree (to get the block data).


# Drawbacks
[drawbacks]: #drawbacks

- Restarts can cause `zebrad` to redownload up to the last one hundred blocks
  it verified in the best chain, and potentially some recent side-chain blocks.

- The service interface puts some extra responsibility on callers to ensure
  it is used correctly and does not verify the usage is correct at compile
  time.

- the service API is verbose and requires manually unwrapping enums

- We do not handle reorgs the same way zcashd does, and could in theory need
  to delete our entire on disk state and resync the chain in some
  pathological reorg cases.
- testnet rollbacks are infrequent, but possible, due to bugs in testnet
  releases. Each testnet rollback will require additional state service code.
