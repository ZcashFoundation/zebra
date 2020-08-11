# Parallel Verification

- Feature Name: parallel_verification
- Start Date: 2020-07-27
- Design PR: [ZcashFoundation/zebra#763](https://github.com/ZcashFoundation/zebra/pull/763)
- Zebra Issue: [ZcashFoundation/zebra#682](https://github.com/ZcashFoundation/zebra/issues/682)

# Summary
[summary]: #summary

Zebra verifies blocks in several stages, most of which can be executed in
parallel.

We use several different design patterns to enable this parallelism:
* We download blocks and start verifying them in parallel,
* We batch signature and proof verification using verification services, and
* We defer data dependencies until just before the block is committed to the
  state (for details, see the
  [Deferred Verification Using Constraints RFC](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/rfcs/0003-constraint-verification.md)).

# Motivation
[motivation]: #motivation

Zcash (and Bitcoin) are designed to verify each block in sequence, starting
from the genesis block. But during the initial sync, and when restarting with
an older state, this process can be quite slow.

By deferring data dependencies, we can partially verify multiple blocks in
parallel.

By parallelising block and transaction verification, we can use multithreading
and batch verification for signatures, proofs, scripts, and hashes.

# Definitions
[definitions]: #definitions

Blockchain:
* **chain fork:** Zcash is implemented using a tree of blocks. Each block has a
                  single previous block, and zero to many next blocks. A chain
                  fork consists of a tip and all its previous blocks, back to
                  the genesis block.
* **genesis:** The root of the tree of blocks is called the genesis block. It has
               no previous block.
* **tip:** A block which has no next block is called a tip. Each chain fork can
           be identified using its tip.

Data:
* **data dependency:** Information contained in the previous block and its
                       chain fork, which is required to verify the current block.
* **state:** The set of verified blocks. The state may also cache some dependent
             data, so that we can efficienty verify subsequent blocks.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

**TODO:** write guide after details have stabilised

> Explain the proposal as if it was already included in the project and you were teaching it to another Zebra programmer. That generally means:
>

> - Introducing new named concepts.
> - Explaining the feature largely in terms of examples.
> - Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra. It should explain the impact as concretely as possible.
> - If applicable, describe the differences between teaching this to existing Zebra programmers and new Zebra programmers.
>
> For implementation-oriented RFCs (e.g. for compiler internals), this section should focus on how compiler contributors should think about the change, and give examples of its concrete impact.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Verification Stages
[verification-stages]: #verification-stages

In Zebra, verification occurs in the following stages:
* **Structural Verification:** Raw block data is parsed into a block header and
  transactions. Invalid data is not representable in these structures:
  deserialization (parsing) can fail, but serialization always succeeds.
* **Semantic Verification:** Parsed block fields are verified, based on their
  data dependencies:
  * Context-free fields have no data dependencies, so they can be verified as
    needed.
  * Fields with simple data dependencies defer that dependency as long as
    possible, so they can perform more verification in parallel. Then they await
    the required data, which is typically the previous block. (And potentially
    older blocks in its chain fork.)
  * Fields with complex data dependencies require their own parallel verification
    designs. These designs are out of scope for this RFC.
* **State Updates:** After a block is verified, it is added to the state. The
  details of state updates, and their interaction with semantic verification,
  are out of scope for this RFC.

This RFC focuses on Semantic Verification, and the design patterns that enable
blocks to be verified in parallel.

## Verification Interfaces
[verification-interfaces]: #verification-interfaces

Verifcation is implemented by the following traits and services:
* **Structural Verification:**
  * `zebra_network::init`: Provides a downloader service that accepts a
    `BlockHeaderHash` request, and parses the peer response into a `Block`
    struct.
  * `zebra_chain::ZcashDeserialize`: A trait for parsing consensus-critical
    data structures from a byte buffer.
* **Semantic Verification:**
  * `ChainVerifier`: Provides a verifier service that accepts a `Block` request,
    performs verification on the block, and responds with a `BlockHeaderHash` on
    success.
  * Internally, the `ChainVerifier` selects between a `CheckpointVerifier` for
    blocks that are within the checkpoint range, and a `BlockVerifier` for
    recent blocks.
* **State Updates:**
  * `zebra_state::init`: Provides the state update service, which accepts
    requests to add blocks to the state.

### Checkpoint Verification
[checkpoint-verification]: #checkpoint-verification

The `CheckpointVerifier` performs rapid verification of blocks, based on a set
of hard-coded checkpoints. Each checkpoint hash can be used to verify all the

previous blocks, back to the genesis block. So Zebra can skip almost all
verification for blocks in the checkpoint range.

The `CheckpointVerifier` uses an internal queue to store pending blocks.
Checkpoint verification is cheap, so it is implemented using non-async
functions within the CheckpointVerifier service.

Here is how the `CheckpointVerifier` implements each verification stage:

* **Structural Verification:**
  * *As Above:* the `CheckpointVerifier` accepts parsed `Block` structs.
* **Semantic Verification:**
  * `check_height`: makes sure the block height is within the unverified
    checkpoint range, and adds the block to its internal queue.
  * `target_checkpoint_height`: Checks for a continuous range of blocks from
    the previous checkpoint to a subsequent checkpoint. If the chain is
    incomplete, returns a future, and waits for more blocks. If the chain is
    complete, assumes that the `previous_block_hash` fields of these blocks
    form an unbroken chain from checkpoint to checkpoint, and starts
    processing the checkpoint range. (This constraint is an implicit part of
    the `CheckpointVerifier` design.)
  * `process_checkpoint_range`: makes sure that the blocks in the checkpoint
    range have an unbroken chain of previous block hashes.
* **State Updates:**
  * *As Above:* the `CheckpointVerifier` returns success to the `ChainVerifier`,
    which sends verified `Block`s to the state service.
  * The state service implements the chain order consensus rule, which makes sure
    each block is added to state after its previous block.

### Block Verification
[block-verification]: #block-verification

The `BlockVerifier` performs detailed verification of recent blocks, in parallel.

Here is how the `BlockVerifier` implements each verification stage:

* **Structural Verification:**
  * *As Above:* the `BlockVerifier` accepts parsed `Block` structs.
* **Semantic Verification:**
  * *As Above:* verifies each field in the block. Defers any data dependencies as
    long as possible, then awaits those data dependencies.
  * Note: The context-free, deferred, and dependent verification stages can be
    implemented as separate async functions.
* **State Updates:**
  * *As Above:* the `BlockVerifier` returns success to the `ChainVerifier`,
    which sends verified `Block`s to the state service.
  * The state service implements the chain order consensus rule, which makes sure
    each block is added to state after its previous block.

# Drawbacks
[drawbacks]: #drawbacks

**TODO:** this design is a bit complicated, but we think it's necessary to
          achieve our goals.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

**TODO:** expand on notes below

- Why is this design the best in the space of possible designs?
  - It's a good enough design to build upon
- What other designs have been considered and what is the rationale for not choosing them?
  - Serial verification
  - Effectively single-threaded
- What is the impact of not doing this?
  - Verification is slow, we can't batch or parallelise some parts of the
    verification

# Prior art
[prior-art]: #prior-art

**TODO:**
  - zcashd
    - serial block verification
    - Zebra implements the same consensus rules, but a different design
  - tower

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- [ ] Is this design good enough to use as a framework for future RFCs?
- [ ] Does this design require any changes to the current implementation?
  - Implement chain order consensus rule in state (previous block hash only)
  - Split `BlockVerifier.call` into context-free, deferred, and dependent async
    functions?

Out of Scope:
- What is the most efficient design for parallel verification?
  - (Optimisations are out of scope.)

- How is each specific field verified?
- How do we verify fields with complex data dependencies?
- How does verification change with different network upgrades?

- How do multiple chains work, in detail?
- How do state updates work, in detail?

# Future possibilities
[future-possibilities]: #future-possibilities

**TODO:**
  - Constraint Verification RFC - state and data dependency details
  - Separate RFCs for complex data dependencies
  - Optimisations for parallel verification
