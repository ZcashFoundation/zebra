# Treestate

- Feature Name: treestate
- Start Date: 2020-08-31
- Design PR: [ZcashFoundation/zebra#983](https://github.com/ZcashFoundation/zebra/issues/983)
- Zebra Issue: [ZcashFoundation/zebra#958](https://github.com/ZcashFoundation/zebra/issues/958)

# Summary
[summary]: #summary

To validate blocks involving shielded transactions, we have to check the
computed treestate from the included transactions against the block header
metadata. This document describes how we compute and manage that data, assuming
a finalized state service as described in the [State Updates RFC](https://zebra.zfnd.org/dev/rfcs/0005-state-updates.md).


# Motivation
[motivation]: #motivation

Block validation requires checking that the treestate of the block (consisting
of the note commitment tree and nullifier set) is consistent with the metadata
we have in the block header (the root of the note commitment tree).


# Definitions
[definitions]: #definitions

Many terms used here are defined in the [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf)

**notes**: Represents that a value is spendable by the recipient who holds the
spending key corresponding to a given shielded payment address.

**nullifiers**: Revealed by Spend descriptions when its associated note is spent.

**nullifier set**: The set of unique nullifiers revealed by any transactions
within a block. Nullifiers are enforced to be unique within a valid block chain
by commiting to previous treestates in Spend descriptions, in order to prevent
double-spends.

**note commitments**: Pedersen commitment to the values consisting a note. One
should not be able to construct a note from its commitment.

**note position**: The index of a note commitment at the leafmost layer.

**note commitment tree**: An incremental Merkle tree of fixed depth used to
store note commitments that JoinSplit transfers or Spend transfers produce. It
is used to express the existence of value and the capability to spend it. It is
not the job of this tree to protect against double-spending, as it is
append-only: that's what the nullifier set is for.

**root**: Layer 0 of a note commitment tree associated with each treestate.

**anchor**: A Merkle tree root of a note commitment tree. It uniquely identies a
note commitment tree state given the assumed security properties of the Merkle
tree’s hash function.  Since the nullier set is always updated together with the
note commitment tree, this also identies a particular state of the associated
nullier set.

**joinsplits**:

**spend descriptions**:

**output descriptions**:



# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

As the transactions within a block are parsed, Sapling shielded transactions
including Spend descriptions and Output descriptions describe the spending and
creation of Zcash notes. A Spend description includes an anchor,


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

