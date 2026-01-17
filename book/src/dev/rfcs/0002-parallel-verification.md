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

- We download blocks and start verifying them in parallel,
- We batch signature and proof verification using verification services, and
- We defer data dependencies until just before the block is committed to the
  state (see the detailed design RFCs).

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

- **chain fork:** Zcash is implemented using a tree of blocks. Each block has a
  single previous block, and zero to many next blocks. A chain
  fork consists of a tip and all its previous blocks, back to
  the genesis block.
- **genesis:** The root of the tree of blocks is called the genesis block. It has
  no previous block.
- **tip:** A block which has no next block is called a tip. Each chain fork can
  be identified using its tip.

Data:

- **consensus rule:** A protocol rule which all nodes must apply consistently,
  so they can converge on the same chain fork.
- **context-free:** Consensus rules which do not have a data dependency on
  previous blocks.
- **data dependency:** Information contained in the previous block and its
  chain fork, which is required to verify the current block.
- **state:** The set of verified blocks. The state might also cache some
  dependent data, so that we can efficiently verify subsequent blocks.

Verification Stages:

<!-- The verification stages are listed in chronological order -->

- **structural verification:** Parsing raw bytes into the data structures defined
  by the protocol.
- **semantic verification:** Verifying the consensus rules on the data structures
  defined by the protocol.
- **contextual verification:** Verifying the current block, once its data
  dependencies have been satisfied by a verified
  previous block. This verification might also use
  the cached state corresponding to the previous
  block.

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

In Zebra, we want to verify blocks in parallel. Some fields can be verified
straight away, because they don't depend on the output of previous blocks.
But other fields have **data dependencies**, which means that we need previous
blocks before we can fully validate them.

If we delay checking some of these data dependencies, then we can do more of
the verification in parallel.

## Example: BlockHeight

[block-height]: #block-height

Here's how Zebra can verify the different Block Height consensus rules in
parallel:

**Structural Verification:**

1. Parse the Block into a BlockHeader and a list of transactions.

**Semantic Verification: No Data Dependencies:**

1. Check that the first input of the first transaction in the block is a coinbase
   input with a valid block height in its data field.

**Semantic Verification: Deferring a Data Dependency:**

1. Verify other consensus rules that depend on Block Height, assuming that the
   Block Height is correct. For example, many consensus rules depend on the
   current Network Upgrade, which is determined by the Block Height. We verify
   these consensus rules, assuming the Block Height and Network Upgrade are
   correct.

**Contextual Verification:**

1. Submit the block to the state for contextual verification. When it is ready to
   be committed (it may arrive before the previous block), check all deferred
   constraints, including the constraint that the block height of this block is
   one more than the block height of its parent block. If all constraints are
   satisfied, commit the block to the state. Otherwise, reject the block as
   invalid.

## Zebra Design

[zebra-design]: #zebra-design

### Design Patterns

[design-patterns]: #design-patterns

When designing changes to Zebra verification, use these design patterns:

- perform context-free verification as soon as possible,
  (that is, verification which has no data dependencies on previous blocks),
- defer data dependencies as long as possible, then
- check the data dependencies.

### Minimise Deferred Data

[minimise-deferred-data]: #minimise-deferred-data

Keep the data dependencies and checks as simple as possible.

For example, Zebra could defer checking both the Block Height and Network Upgrade.

But since the Network Upgrade depends on the Block Height, we only need to defer
the Block Height check. Then we can use all the fields that depend on the
Block Height, as if it is correct. If the final Block Height check fails, we will
reject the entire block, including all the verification we performed using the
assumed Network Upgrade.

### Implementation Strategy

[implementation-strategy]: #implementation-strategy

When implementing these designs, perform as much verification as possible, await
any dependencies, then perform the necessary checks.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

## Verification Stages

[verification-stages]: #verification-stages

In Zebra, verification occurs in the following stages:

- **Structural Verification:** Raw block data is parsed into a block header and
  transactions. Invalid data is not representable in these structures:
  deserialization (parsing) can fail, but serialization always succeeds.
- **Semantic Verification:** Parsed block fields are verified, based on their
  data dependencies:
  - Context-free fields have no data dependencies, so they can be verified as
    needed.
  - Fields with simple data dependencies defer that dependency as long as
    possible, so they can perform more verification in parallel. Then they await
    the required data, which is typically the previous block. (And potentially
    older blocks in its chain fork.)
  - Fields with complex data dependencies require their own parallel verification
    designs. These designs are out of scope for this RFC.
- **Contextual Verification:** After a block is verified, it is added to the state. The
  details of state updates, and their interaction with semantic verification,
  are out of scope for this RFC.

This RFC focuses on Semantic Verification, and the design patterns that enable
blocks to be verified in parallel.

## Verification Interfaces

[verification-interfaces]: #verification-interfaces

Verification is implemented by the following traits and services:

- **Structural Verification:**
  - `zebra_chain::ZcashDeserialize`: A trait for parsing consensus-critical
    data structures from a byte buffer.
- **Semantic Verification:**
  - `ChainVerifier`: Provides a verifier service that accepts a `Block` request,
    performs verification on the block, and responds with a `block::Hash` on
    success.
  - Internally, the `ChainVerifier` selects between a `CheckpointVerifier` for
    blocks that are within the checkpoint range, and a `BlockVerifier` for
    recent blocks.
- **Contextual Verification:**
  - `zebra_state::init`: Provides the state update service, which accepts
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

- **Structural Verification:**
  - _As Above:_ the `CheckpointVerifier` accepts parsed `Block` structs.
- **Semantic Verification:**
  - `check_height`: makes sure the block height is within the unverified
    checkpoint range, and adds the block to its internal queue.
  - `target_checkpoint_height`: Checks for a continuous range of blocks from
    the previous checkpoint to a subsequent checkpoint. If the chain is
    incomplete, returns a future, and waits for more blocks. If the chain is
    complete, assumes that the `previous_block_hash` fields of these blocks
    form an unbroken chain from checkpoint to checkpoint, and starts
    processing the checkpoint range. (This constraint is an implicit part of
    the `CheckpointVerifier` design.)
  - `process_checkpoint_range`: makes sure that the blocks in the checkpoint
    range have an unbroken chain of previous block hashes.
- **Contextual Verification:**
  - _As Above:_ the `CheckpointVerifier` returns success to the `ChainVerifier`,
    which sends verified `Block`s to the state service.

### Block Verification

[block-verification]: #block-verification

The `BlockVerifier` performs detailed verification of recent blocks, in parallel.

Here is how the `BlockVerifier` implements each verification stage:

- **Structural Verification:**
  - _As Above:_ the `BlockVerifier` accepts parsed `Block` structs.
- **Semantic Verification:**
  - _As Above:_ verifies each field in the block. Defers any data dependencies as
    long as possible, awaits those data dependencies, then performs data
    dependent checks.
  - Note: Since futures are executed concurrently, we can use the same function
    to:
    - perform context-free verification,
    - perform verification with deferred data dependencies,
    - await data dependencies, and
    - check data dependencies.
      To maximise concurrency, we should write verification functions in this
      specific order, so the awaits are as late as possible.
- **Contextual Verification:**
  - _As Above:_ the `BlockVerifier` returns success to the `ChainVerifier`,
    which sends verified `Block`s to the state service.

## Zcash Protocol Design

[zcash-protocol]: #zcash-protocol

When designing a change to the Zcash protocol, minimise the data dependencies
between blocks.

Try to create designs that:

- Eliminate data dependencies,
- Make the changes depend on a version field in the block header or transaction,
- Make the changes depend on the current Network Upgrade, or
- Make the changes depend on a field in the current block, with an additional
  consensus rule to check that field against previous blocks.

When making decisions about these design tradeoffs, consider:

- how the data dependency could be deferred, and
- the CPU cost of the verification - if it is trivial, then it does not matter if
  the verification is parallelised.

# Drawbacks

[drawbacks]: #drawbacks

This design is a bit complicated, but we think it's necessary to achieve our
goals.

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

- What makes this design a good design?
  - It enables a significant amount of parallelism
  - It is simpler than some other alternatives
  - It uses existing Rust language facilities, mainly Futures and await/async
- Is this design a good basis for later designs or implementations?
  - We have built a UTXO design on this design
  - We believe we can build "recent blocks" and "chain summary" designs on this
    design
  - Each specific detailed design will need to consider how the relevant data
    dependencies are persisted
- What other designs have been considered and what is the rationale for not choosing them?
  - Serial verification
    - Effectively single-threaded
  - Awaiting data dependencies as soon as they are needed
    - Less parallelism
  - Providing direct access to the state
    - Might cause data races, might be prevented by Rust's ownership rules
    - Higher risk of bugs
- What is the impact of not doing this?
  - Verification is slow, we can't batch or parallelise some parts of the
    verification

# Prior art

[prior-art]: #prior-art

**TODO: expand this section**

- zcashd
  - serial block verification
  - Zebra implements the same consensus rules, but a different design
- tower

# Unresolved questions

[unresolved-questions]: #unresolved-questions

- [ ] Is this design good enough to use as a framework for future RFCs?
- [ ] Does this design require any changes to the current implementation?
  - [ ] Implement block height consensus rule (check previous block hash and height)
  - [ ] Check that the `BlockVerifier` performs checks in the following order:
    - verification, deferring dependencies as needed,
    - await dependencies,
    - check deferred data dependencies

Out of Scope:

- What is the most efficient design for parallel verification?
  - (Optimisations are out of scope.)

- How is each specific field verified?
- How do we verify fields with complex data dependencies?
- How does verification change with different network upgrades?

- How do multiple chains work, in detail?
- How do state updates work, in detail?

- Moving the verifiers into the state service

# Future possibilities

[future-possibilities]: #future-possibilities

- Separate RFCs for other data dependencies
  - Recent blocks
  - Overall chain summaries (for example, total work)
  - Reorganisation limit: multiple chains to single chain transition
- Optimisations for parallel verification

# Implementation Notes

[implementation-notes]: #implementation-notes

> **Last validated:** December 2025

This RFC has been fully implemented. The following notes document naming
evolution and implementation details:

## Type Name Changes

| RFC Name | Implementation Name | Location |
|----------|-------------------|----------|
| `ChainVerifier` | `BlockVerifierRouter` | `zebra-consensus/src/router.rs` |
| `BlockVerifier` | `SemanticBlockVerifier` | `zebra-consensus/src/block.rs` |
| `CheckpointVerifier` | `CheckpointVerifier` (unchanged) | `zebra-consensus/src/checkpoint.rs` |

## Verification Stages (Verified)

All three verification stages are implemented as described:

1. **Structural Verification**: `ZcashDeserialize` in `zebra-chain/src/serialization/`
2. **Semantic Verification**: `SemanticBlockVerifier` and `transaction::Verifier`
3. **Contextual Verification**: State service in `zebra-state/src/service.rs`

## Deferred Dependencies Pattern

The deferred dependencies pattern is implemented using types like:

- `DeferredPoolBalanceChange` for value pool updates
- Async checks via `tower-batch-control` for batch verification

## Entry Point

```rust
let verifier = zebra_consensus::init(config, network, state_service).await;
// Returns BlockVerifierRouter that routes to checkpoint or semantic verification
```
