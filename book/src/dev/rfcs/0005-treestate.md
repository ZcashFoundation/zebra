# Treestate

- Feature Name: treestate
- Start Date: 2020-08-31
- Design PR:
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


# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation


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

