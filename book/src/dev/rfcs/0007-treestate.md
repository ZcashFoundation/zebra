# Treestate

- Feature Name: treestate
- Start Date: 2020-08-31
- Design PR: [ZcashFoundation/zebra#983](https://github.com/ZcashFoundation/zebra/issues/983)
- Zebra Issue: [ZcashFoundation/zebra#958](https://github.com/ZcashFoundation/zebra/issues/958)

# Summary

[summary]: #summary

To validate blocks involving shielded transactions, we have to check the
computed treestate from the included transactions against the block header
metadata (for Sapling and Orchard) or previously finalized state (for Sprout).
This document describes how we compute and manage that data, assuming a finalized
state service as described in the [State Updates RFC](./0005-state-updates.md).

# Motivation

[motivation]: #motivation

Block validation requires checking that the treestate of the block (consisting
of the note commitment tree and nullifier set) is consistent with the metadata
we have in the block header (the root of the note commitment tree) or previously
finalized state (for Sprout).

# Definitions

[definitions]: #definitions

## Common Definitions

Many terms used here are defined in the [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf)

**notes**: Represents a value bound to a shielded payment address (public key)
which is spendable by the recipient who holds the spending key corresponding to
a given shielded payment address.

**nullifiers**: A value that prevents double-spending of a shielded payment.
Revealed by `Spend` descriptions when its associated `Note` is spent.

**nullifier set**: The set of unique `Nullifier`s revealed by any `Transaction`s
within a `Block`. `Nullifier`s are enforced to be unique within a valid block chain
by committing to previous treestates in `Spend` descriptions, in order to prevent
double-spends.

**note commitments**: Pedersen commitment to the values consisting a `Note`. One
should not be able to construct a `Note` from its commitment.

**note commitment tree**: An incremental Merkle tree of fixed depth used to
store `NoteCommitment`s that `JoinSplit` transfers or `Spend` transfers produce. It
is used to express the existence of value and the capability to spend it. It is
not the job of this tree to protect against double-spending, as it is
append-only: that's what the `Nullifier` set is for.

**note position**: The index of a `NoteCommitment` at the leafmost layer,
counting leftmost to rightmost. The [position in the tree is determined by the
order of transactions in the block](https://zips.z.cash/protocol/protocol.pdf#transactions).

**root**: The layer 0 node of a Merkle tree.

**anchor**: A Merkle tree root of a `NoteCommitment` tree. It uniquely
identifies a `NoteCommitment` tree state given the assumed security properties
of the Merkle tree’s hash function. Since the `Nullifier` set is always updated
together with the `NoteCommitment` tree, this also identifies a particular state
of the associated `Nullifier` set.

## Sprout Definitions

**joinsplit**: A shielded transfer that can spend Sprout `Note`s and transparent
value, and create new Sprout `Note`s and transparent value, in one Groth16 proof
statement.

## Sapling Definitions

**spend descriptions**: A shielded Sapling transfer that spends a `Note`. Includes
an anchor of some previous `Block`'s `NoteCommitment` tree.

**output descriptions**: A shielded Sapling transfer that creates a
`Note`. Includes the u-coordinate of the `NoteCommitment` itself.

## Orchard Definitions

**action descriptions**: A shielded Orchard transfer that spends and/or creates a
`Note`. Does not include an anchor, because that is encoded once in the
`anchorOrchard` field of a V5 `Transaction`.

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

## Common Processing for All Protocols

As `Block`s are validated, the `NoteCommitment`s revealed by all the transactions
within that block are used to construct `NoteCommitmentTree`s, with the
`NoteCommitment`s aligned in their note positions in the bottom layer of the
Sprout or Sapling tree from the left-most leaf to the right-most in
`Transaction` order in the `Block`. So the Sprout `NoteCommitment`s revealed by
the first `JoinSplit<Groth16>` in a block would take note position 0 in the Sprout
note commitment tree, for example. Once all the transactions in a block are
parsed and the notes for each tree collected in their appropriate positions, the
root of each tree is computed. While the trees are being built, the respective
block nullifier sets are updated in memory as note nullifiers are revealed. If
the rest of the block is validated according to consensus rules, that root is
committed to its own data structure via our state service (Sprout anchors,
Sapling anchors). Sapling block validation includes comparing the specified
FinalSaplingRoot in its block header to the root of the Sapling `NoteCommitment`
tree that we have just computed to make sure they match.

## Sprout Processing

For Sprout, we must compute/update interstitial `NoteCommitmentTree`s between
`JoinSplit`s that may reference an earlier one's root as its anchor. If we do
this at the transaction layer, we can iterate through all the `JoinSplit`s and
compute the Sprout `NoteCommitmentTree` and nullifier set similar to how we do
the Sapling ones as described below, but at each state change (ie,
per-`JoinSplit`) we note the root and cache it for lookup later. As the
`JoinSplit`s are validated without context, we check for its specified anchor
amongst the interstitial roots we've already calculated (according to the spec,
these interstitial roots don't have to be finalized or the result of an
independently validated `JoinSplit`, they just must refer to any prior `JoinSplit`
root in the same transaction). So we only have to wait for our previous root to
be computed via any of our candidates, which in the worst case is waiting for
all of them to be computed for the last `JoinSplit`. If our `JoinSplit`s defined
root pops out, that `JoinSplit` passes that check.

## Sapling Processing

As the transactions within a block are parsed, Sapling shielded transactions
including `Spend` descriptions and `Output` descriptions describe the spending and
creation of Zcash Sapling notes. `Spend` descriptions specify an anchor, which
references a previous `NoteCommitment` tree root. This is a previous block's anchor
as defined in their block header. This is convenient because we can query our state
service for previously finalized Sapling block anchors, and if they are found, then
that [consensus check](https://zips.z.cash/protocol/canopy.pdf#spendsandoutputs)
has been satisfied and the `Spend` description can be validated independently.

For Sapling, at the block layer, we can iterate over all the transactions in
order and if they have `Spend`s and/or `Output`s, we update our Nullifer set for
the block as nullifiers are revealed in `Spend` descriptions, and update our note
commitment tree as `NoteCommitment`s are revealed in `Output` descriptions, adding
them as leaves in positions according to their order as they appear transaction
to transaction, output to output, in the block. This can be done independent of
the transaction validations. When the Sapling transactions are all validated,
the `NoteCommitmentTree` root should be computed: this is the anchor for this
block.

### Anchor Validation Across Network Upgrades

For Sapling and Blossom blocks, we need to check that this root matches
the `RootHash` bytes in this block's header, as the `FinalSaplingRoot`. Once all
other consensus and validation checks are done, this will be saved down to our
finalized state to our `sapling_anchors` set, making it available for lookup by
other Sapling descriptions in future transactions.

In Heartwood and Canopy, the rules for final Sapling roots are modified to support
empty blocks by allowing an empty subtree hash instead of requiring the root to
match the previous block's final Sapling root when there are no Sapling transactions.

In NU5, the rules are further extended to include Orchard note commitment trees,
with similar logic applied to the `anchorOrchard` field in V5 transactions.

## Orchard Processing

For Orchard, similar to Sapling, action descriptions can spend and create notes.
The anchor is specified at the transaction level in the `anchorOrchard` field of
a V5 transaction. The process follows similar steps to Sapling for validation and
inclusion in blocks.

## Block Finalization

To finalize the block, the Sprout, Sapling, and Orchard treestates are the ones
resulting from the last transaction in the block, and determines the respective
anchors that will be associated with this block as we commit it to our finalized
state. The nullifiers revealed in the block will be merged with the existing ones
in our finalized state (ie, it should strictly grow over time).

## State Management

### Orchard

- There is a single copy of the latest Orchard Note Commitment Tree for the
  finalized tip.
- When finalizing a block, the finalized tip is updated with a serialization of
  the latest Orchard Note Commitment Tree. (The previous tree should be deleted as
  part of the same database transaction.)
- Each non-finalized chain gets its own copy of the Orchard note commitment tree,
  cloned from the note commitment tree of the finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Orchard note commitment
  tree is updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip, the Orchard tree
  state is restored to its previous state before the block was added. This involves
  either keeping a reference to the previous state or recalculating from the fork
  point.

### Sapling

- There is a single copy of the latest Sapling Note Commitment Tree for the
  finalized tip.
- When finalizing a block, the finalized tip is updated with a serialization of
  the Sapling Note Commitment Tree. (The previous tree should be deleted as part
  of the same database transaction.)
- Each non-finalized chain gets its own copy of the Sapling note commitment tree,
  cloned from the note commitment tree of the finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Sapling note commitment
  tree is updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip, the Sapling tree
  state is restored to its previous state, similar to the Orchard process. This
  involves either maintaining a history of tree states or recalculating from the
  fork point.

### Sprout

- Every finalized block stores a separate copy of the Sprout note commitment
  tree (😿), as of that block.
- When finalizing a block, the Sprout note commitment tree for that block is stored
  in the state. (The trees for previous blocks also remain in the state.)
- Every block in each non-finalized chain gets its own copy of the Sprout note
  commitment tree. The initial tree is cloned from the note commitment tree of the
  finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Sprout note commitment
  tree is cloned, then updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip, the trees for each
  block are deleted, along with that block.

We can't just compute a fresh tree with just the note commitments within a block,
we are adding them to the tree referenced by the anchor, but we cannot update that
tree with just the anchor, we need the 'frontier' nodes and leaves of the
incremental merkle tree.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

The implementation involves several key components:

1. **Incremental Merkle Trees**: We use the `incrementalmerkletree` crate to
   implement the note commitment trees for each shielded pool.

2. **Nullifier Storage**: We maintain nullifier sets in RocksDB to efficiently
   check for duplicates.

3. **Tree State Management**:
   - For finalized blocks, we store the tree states in RocksDB.
   - For non-finalized chains, we keep tree states in memory.

4. **Anchor Verification**:
   - For Sprout: we check anchors against our stored Sprout tree roots.
   - For Sapling: we compare the computed root against the block header's
     `FinalSaplingRoot`.
   - For Orchard: we validate the `anchorOrchard` field in V5 transactions.

5. **Re-insertion Prevention**: Our implementation should prevent re-inserts
   of keys that have been deleted from the database, as this could lead to
   inconsistencies. The state service tracks deletion events and validates insertion
   operations accordingly.

# Drawbacks

[drawbacks]: #drawbacks

1. **Storage Requirements**: Storing separate tree states (especially for Sprout)
   requires significant disk space.

2. **Performance Impact**: Computing and verifying tree states can be
   computationally expensive, potentially affecting sync performance.

3. **Implementation Complexity**: Managing multiple tree states across different
   protocols adds complexity to the codebase.

4. **Fork Handling**: Maintaining correct tree states during chain reorganizations
   requires careful handling.

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

We chose this approach because:

1. **Protocol Compatibility**: Our implementation follows the Zcash protocol
   specification requirements for handling note commitment trees and anchors.

2. **Performance Optimization**: By caching tree states, we avoid recomputing
   them for every validation operation.

3. **Memory Efficiency**: For non-finalized chains, we only keep necessary tree
   states in memory.

4. **Scalability**: The design scales with chain growth by efficiently managing
   storage requirements.

Alternative approaches considered:

1. **Recompute Trees On-Demand**: Instead of storing tree states, recompute them
   when needed. This would save storage but significantly impact performance.

2. **Single Tree State**: Maintain only the latest tree state and recompute for
   historical blocks. This would simplify implementation but make historical validation harder.

3. **Full History Storage**: Store complete tree states for all blocks. This would optimize
   validation speed but require excessive storage.

# Prior art

[prior-art]: #prior-art

1. **Zcashd**: Uses similar concepts but with differences in implementation details,
   particularly around storage and concurrency.

2. **Lightwalletd**: Provides a simplified approach to tree state management focused
   on scanning rather than full validation.

3. **Incrementalmerkletree Crate**: Our implementation leverages this existing Rust
   crate for efficient tree management.

# Unresolved questions

[unresolved-questions]: #unresolved-questions

1. **Optimization Opportunities**: Are there further optimizations we can make to reduce
   storage requirements while maintaining performance?

2. **Root Storage**: Should we store the `Root` hash in `sprout_note_commitment_tree`,
   and use it to look up the complete tree state when needed?

3. **Re-insertion Prevention**: What's the most efficient approach to prevent re-inserts
   of deleted keys?

4. **Concurrency Model**: How do we best handle concurrent access to tree states during
   parallel validation?

# Future possibilities

[future-possibilities]: #future-possibilities

1. **Pruning Strategies**: Implement advanced pruning strategies for historical tree states
   to reduce storage requirements.

2. **Parallelization**: Further optimize tree state updates for parallel processing.

3. **Checkpoint Verification**: Use tree states for efficient checkpoint-based verification.

4. **Light Client Support**: Leverage tree states to support Zebra-based light clients with
   efficient proof verification.

5. **State Storage Optimization**: Investigate more efficient serialization formats and storage
   mechanisms for tree states.
