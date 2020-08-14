# State Updates

- Feature Name: state_updates
- Start Date: 2020-08-14
- Design PR: XXX
- Zebra Issue: XXX

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

* **chain reorganization**: Occurs when a new best chain is found and the
  previous best chain becomes a side chain.

* **orphaned block**: A block which is no longer included in the best chain.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

XXX fill in after writing other details

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
while `zcashd` limits reorganizations to 100 blocks.

This difference means that in Bitcoin, chain state only has probabilistic
finality, while in Zcash, chain state is final once it is beyond the reorg
limit. To simplify our implementation, we split the representation of the
state data at the finality boundary provided by the reorg limit.

State data from blocks *above* the reorg limit is stored in-memory using
immutable data structures from the `im` crate. State data from blocks *below*
the reorg limit is stored persistently using `sled`. This allows a
simplification of our state handling, because only final data is persistent.

We choose `im` because it provides best-in-class manipulation of persistent
data structures. We choose `sled` because of its ease of integration and API
simplicity.

One downside of this design is that restarting the node loses the last 100
blocks, but node restarts are relatively infrequent and a short re-sync is
cheap relative to the cost of additional implementation complexity.

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

In summary:

- **Sled reads** may be done synchronously (in `call`) or asynchronously (in
  the `Future`), depending on the context;

- **Sled writes** must be done synchronously (in `call`).

## In-memory data structures
[in-memory]: #in-memory

At a high level, the in-memory data structures store a collection of chains,
each rooted at the highest finalized block. Each chain consists of a map from
heights to blocks. Chains are stored using an ordered map from difficulty to
chains, so that the map ordering is the ordering of best to worst chains.

- XXX fill in details on exact types

- XXX work out whether we should store extra data (e.g., a HashSet of UTXOs
  spent by some block etc.) to speed up checks.

When a new block extends the best chain past 100 blocks, the old root is
removed from the in-memory state and committed to sled.

## Sled data structures
[sled]: #sled

Sled provides a persistent, thread-safe `BTreeMap<&[u8], &[u8]>`. Each map is
a distinct "tree". Keys are sorted using lex order on byte strings, so
integer values should be stored using big-endian encoding (so that the lex
order on byte strings is the numeric ordering).

We use the following Sled trees:

| Tree                |                  Keys |                              Values |
|---------------------|-----------------------|-------------------------------------|
| `blocks_by_hash`    | `BlockHeaderHash`     | `Block`                             |
| `hash_by_height`    | `BE32(height)`        | `BlockHeaderHash`                   |
| `tx_by_hash`        | `TransactionHash`     | `BlockHeaderHash || BE32(tx_index)` |
| `utxo_by_outpoint`  | `OutPoint`            | `TransparentOutput`                 |

Zcash structures are encoded using `ZcashSerialize`/`ZcashDeserialize`.

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

### `Request::CommitBlock(Arc<Block>)`
[request-commit-block]: #request-commit-block

Performs contextual validation of the given block, committing it to the state
if successful. Returns `Response::Added(BlockHeaderHash)` with the hash of
the newly committed block or an error.

If the parent block is not committed, add the block to an internal queue for
future processing.

Otherwise, attempt to perform contextual validation checks and the commit
the given block to the state. The exact list of contextual validation checks
will be specified in a later RFC. If contextual validation checks succeed,
the new block is added to one of the in-memory chains. If the resulting chain
is longer than 100 blocks, the oldest block is now past the reorg limit, so
it is removed from the in-memory chain and committed to sled as described
below.

Finally, process any queued children of the newly committed block the same way.

### `Request::CommitFinalizedBlock`
[request-commit-finalized-block]: #request-finalized-block

Commits a finalized block to the sled state, skipping contextual validation.
The block's parent must be the current sled tip. This is exposed for use in
checkpointing, which produces in-order finalized blocks. Returns
`Response::Added(BlockHeaderHash)` with the hash of the committed block if
successful.

This should be implemented as a wrapper around a function also called by
[`Request::CommitBlock`](#request-commit-block), which should:

1. Obtain the highest entry of `hash_by_height` as `(old_height, old_tip)`.
Check that `block`'s parent hash is `old_tip` and its height is
`old_height+1`, or error.  This check is performed as defense-in-depth
to prevent database corruption, but it is the caller's responsibility to
commit finalized blocks in order.

2. Insert `(block_hash, block)` into `blocks_by_hash` and
   `(BE32(height), block_hash)` into `hash_by_height`.

3. Iterate over the enumerated transactions in the block. For each transaction:
   1. Insert `(transaction_hash, block_hash || BE32(tx_index))` to `tx_by_hash`;
   2. For each `TransparentInput::PrevOut { outpoint, .. }` in the
      transaction's `inputs()`, remove `outpoint` from `utxo_by_output`.
   3. For each `output` in the transaction's `outputs()`, construct the
      `outpoint` that identifies it, and insert `(outpoint, output)` into `utxo_by_output`.
   These updates can be performed using a sled `Batch`.

### `Request::Depth(BlockHeaderHash)`
[request-depth]: #request-depth

Computes the depth in the best chain of the block identified by the given
hash, returning

- `Response::Depth(Some(depth))` if the block is in the main chain;
- `Response::Depth(None)` otherwise.

### `Request::Tip`
[request-tip]: #request-tip

Returns `Response::Tip(BlockHeaderHash)` with the current best chain tip.

### `Request::BlockLocator`
[request-block-locator]: #request-block-locator

- XXX fill in

### `Request::Transaction(TransactionHash)`
[request-transaction]: #request-transaction

- XXX fill in

### `Request::Block(BlockHeaderHash)`
[request-block]: #request-block

- XXX fill in

# Drawbacks
[drawbacks]: #drawbacks

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

# Prior art
[prior-art]: #prior-art

# Unresolved questions
[unresolved-questions]: #unresolved-questions

# Future possibilities
[future-possibilities]: #future-possibilities
