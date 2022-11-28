# Treestate

- Feature Name: treestate
- Start Date: 2020-08-31
- Design PR: [ZcashFoundation/zebra#983](https://github.com/ZcashFoundation/zebra/issues/983)
- Zebra Issue: [ZcashFoundation/zebra#958](https://github.com/ZcashFoundation/zebra/issues/958)

# Summary
[summary]: #summary

To validate blocks involving shielded transactions, we have to check the
computed treestate from the included transactions against the block header
metadata (for Sapling and Orchard) or previously finalized state (for Sprout). This document
describes how we compute and manage that data, assuming a finalized state
service as described in the [State Updates RFC](https://zebra.zfnd.org/dev/rfcs/0005-state-updates.md).


# Motivation
[motivation]: #motivation

Block validation requires checking that the treestate of the block (consisting
of the note commitment tree and nullifier set) is consistent with the metadata
we have in the block header (the root of the note commitment tree) or previously
finalized state (for Sprout).


# Definitions
[definitions]: #definitions

TODO: split up these definitions into common, Sprout, Sapling, and possibly Orchard sections

Many terms used here are defined in the [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf)

**notes**: Represents a value bound to a shielded payment address (public key)
which is spendable by the recipient who holds the spending key corresponding to
a given shielded payment address.

**nullifiers**: Revealed by `Spend` descriptions when its associated `Note` is spent.

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
order of transactions in the block](https://zips.z.cash/protocol/canopy.pdf#transactions).

**root**: The layer 0 node of a Merkle tree.

**anchor**: A Merkle tree root of a `NoteCommitment` tree. It uniquely
identifies a `NoteCommitment` tree state given the assumed security properties
of the Merkle tree’s hash function.  Since the `Nullifier` set is always updated
together with the `NoteCommitment` tree, this also identifies a particular state
of the associated `Nullifier` set.

**spend descriptions**: A shielded Sapling transfer that spends a `Note`. Includes
an anchor of some previous `Block`'s `NoteCommitment` tree.

**output descriptions**: A shielded Sapling transfer that creates a
`Note`. Includes the u-coordinate of the `NoteCommitment` itself.

**action descriptions**: A shielded Orchard transfer that spends and/or creates a `Note`. 
Does not include an anchor, because that is encoded once in the `anchorOrchard` 
field of a V5 `Transaction`.



**joinsplit**: A shielded transfer that can spend Sprout `Note`s and transparent
value, and create new Sprout `Note`s and transparent value, in one Groth16 proof
statement.


# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

TODO: split into common, Sprout, Sapling, and probably Orchard sections

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
committed to its own datastructure via our state service (Sprout anchors,
Sapling anchors). Sapling block validation includes comparing the specified
FinalSaplingRoot in its block header to the root of the Sapling `NoteCommitment`
tree that we have just computed to make sure they match.

As the transactions within a block are parsed, Sapling shielded transactions
including `Spend` descriptions and `Output` descriptions describe the spending and
creation of Zcash Sapling notes, and JoinSplit-on-Groth16 descriptions to
transfer/spend/create Sprout notes and transparent value. `JoinSplit` and `Spend`
descriptions specify an anchor, which references a previous `NoteCommitment` tree
root: for `Spend`s, this is a previous block's anchor as defined in their block
header, for `JoinSplit`s, it may be a previous block's anchor or the root
produced by a strictly previous `JoinSplit` description in its transaction. For
`Spend`s, this is convenient because we can query our state service for
previously finalized Sapling block anchors, and if they are found, then that
[consensus check](https://zips.z.cash/protocol/canopy.pdf#spendsandoutputs) has
been satisfied and the `Spend` description can be validated independently. For
`JoinSplit`s, if it's not a previously finalized block anchor, it must be the
treestate anchor of previous `JoinSplit` in this transaction, and we have to wait
for that one to be parsed and its root computed to check that ours is
valid. Luckily, it can only be a previous `JoinSplit` in this transaction, and is
[usually the immediately previous one](zcashd), so the set of candidate anchors
is smaller for earlier `JoinSplit`s in a transaction, but larger for the later
ones. For these `JoinSplit`s, they can be validated independently of their
anchor's finalization status as long as the final check of the anchor is done,
when available, such as at the Transaction level after all the `JoinSplit`s have
finished validating everything that can be validated without the context of
their anchor's finalization state.

So for each transaction, for both `Spend` descriptions and `JoinSplit`s, we can
pre-emptively try to do our consensus check by looking up the anchors in our
finalized set first. For `Spend`s, we then trigger the remaining validation and
when that finishes we are full done with those. For `JoinSplit`s, the anchor
state check may pass early if it's a previous block Sprout `NoteCommitment` tree
root, but it may fail because it's an earlier `JoinSplit`s root instead, so once
the `JoinSplit` validates independently of the anchor, we wait for all candidate
previous `JoinSplit`s in that transaction finish validating before doing the
anchor consensus check again, but against the output treestate roots of earlier
`JoinSplit`s.

Both Sprout and Sapling `NoteCommitment` trees must be computed for the whole
block to validate. For Sprout, we need to compute interstitial treestates in
between `JoinSplit`s in order to do the final consensus check for each/all
`JoinSplit`s, not just for the whole block, as in Sapling.

For Sapling, at the block layer, we can iterate over all the transactions in
order and if they have `Spend`s and/or `Output`s, we update our Nullifer set for
the block as nullifiers are revealed in `Spend` descriptions, and update our note
commitment tree as `NoteCommitment`s are revealed in `Output` descriptions, adding
them as leaves in positions according to their order as they appear transaction
to transaction, output to output, in the block. This can be done independent of
the transaction validations. When the Sapling transactions are all validated,
the `NoteCommitmentTree` root should be computed: this is the anchor for this
block. For Sapling and Blossom blocks, we need to check that this root matches
the `RootHash` bytes in this block's header, as the `FinalSaplingRoot`. Once all
other consensus and validation checks are done, this will be saved down to our
finalized state to our `sapling_anchors` set, making it available for lookup by
other Sapling descriptions in future transactions.
TODO: explain Heartwood, Canopy, NU5 rule variants around anchors.
For Sprout, we must compute/update interstitial `NoteCommitmentTree`s between
`JoinSplit`s that may reference an earlier one's root as its anchor. If we do
this at the transaction layer, we can iterate through all the `JoinSplit`s and
compute the Sprout `NoteCommitmentTree` and nullifier set similar to how we do
the Sapling ones as described above, but at each state change (ie,
per-`JoinSplit`) we note the root and cache it for lookup later. As the
`JoinSplit`s are validated without context, we check for its specified anchor
amongst the interstitial roots we've already calculated (according to the spec,
these interstitial roots don't have to be finalized or the result of an
independently validated `JoinSplit`, they just must refer to any prior `JoinSplit`
root in the same transaction). So we only have to wait for our previous root to
be computed via any of our candidates, which in the worst case is waiting for
all of them to be computed for the last `JoinSplit`. If our `JoinSplit`s defined
root pops out, that `JoinSplit` passes that check.

To finalize the block, the Sprout and Sapling treestates are the ones resulting
from the last transaction in the block, and determines the Sprout and Sapling
anchors that will be associated with this block as we commit it to our finalized
state. The Sprout and Sapling nullifiers revealed in the block will be merged
with the existing ones in our finalized state (ie, it should strictly grow over
time).

## State Management

### Orchard
- There is a single copy of the latest Orchard Note Commitment Tree for the finalized tip.
- When finalizing a block, the finalized tip is updated with a serialization of the latest Orchard Note Commitment Tree. (The previous tree should be deleted as part of the same database transaction.)
- Each non-finalized chain gets its own copy of the Orchard note commitment tree, cloned from the note commitment tree of the finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Orchard note commitment tree is updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip... (TODO)

### Sapling
- There is a single copy of the latest Sapling Note Commitment Tree for the finalized tip.
- When finalizing a block, the finalized tip is updated with a serialization of the Sapling Note Commitment Tree. (The previous tree should be deleted as part of the same database transaction.)
- Each non-finalized chain gets its own copy of the Sapling note commitment tree, cloned from the note commitment tree of the finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Sapling note commitment tree is updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip... (TODO)

### Sprout
- Every finalized block stores a separate copy of the Sprout note commitment tree (😿), as of that block.
- When finalizing a block, the Sprout note commitment tree for that block is stored in the state. (The trees for previous blocks also remain in the state.)
- Every block in each non-finalized chain gets its own copy of the Sprout note commitment tree. The initial tree is cloned from the note commitment tree of the finalized tip or fork root.
- When a block is added to a non-finalized chain tip, the Sprout note commitment tree is cloned, then updated with the note commitments from that block.
- When a block is rolled back from a non-finalized chain tip, the trees for each block are deleted, along with that block.

We can't just compute a fresh tree with just the note commitments within a block, we are adding them to the tree referenced by the anchor, but we cannot update that tree with just the anchor, we need the 'frontier' nodes and leaves of the incremental merkle tree.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation


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
