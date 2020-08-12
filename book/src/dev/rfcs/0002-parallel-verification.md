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
  state (see the detaled design RFCs).

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
* **consensus rule:** A protocol rule which all nodes must apply consistently,
                      so they can converge on the same chain fork.
* **context-free:** Consensus rules which do not have a data dependency on
                    previous blocks.
* **data dependency:** Information contained in the previous block and its
                       chain fork, which is required to verify the current block.
* **semantic verification:** Verifying the consensus rules on the data structures
                             defined by the protocol.
* **state:** The set of verified blocks. The state may also cache some dependent
             data, so that we can efficienty verify subsequent blocks.
* **structural verification:** Parsing raw bytes into the data structures defined
                               by the protocol.

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

Here's how Zebra can verify the different `BlockHeight` consensus rules in
parallel:

**Parsing:**

1. Parse the Block into a BlockHeader and a list of transactions.

**Verification - No Data Dependencies:**

2. Check that all BlockHeights are within the range of valid heights.^
3. Check that the block has exactly one BlockHeight.
4. Check that the BlockHeight is in the first transaction in the Block.

**Verification - Deferring A Data Dependency:**

5. Verify other consensus rules that depend on BlockHeight, assuming that the
   BlockHeight is correct. For example, many consensus rules depend on the
   current Network Upgrade, which is determined by the BlockHeight. We verify
   these consensus rules, assuming the BlockHeight and Network Upgrade are
   correct.

**Verification - Checking A Data Dependency:**

6. Await the previous block. When it arrives, check that the BlockHeight of this
   Block is one more than the BlockHeight of the previous block. If the check
   passes, commit the block to the state. Otherwise, reject the block as invalid.

^ Note that Zebra actually checks the BlockHeight range during parsing. The
  BlockHeight is stored as a compact integer, so out-of-range BlockHeights take
  up additional raw bytes during parsing.

## Zebra Design
[zebra-design]: #zebra-design

### Design Patterns
[design-patterns]: #design-patterns

When designing changes to Zebra verification, use these design patterns:
* perform context-free verification as soon as possible,
  (that is, verification which has no data dependencies on previous blocks),
* defer data dependencies as long as possible, then
* check the data dependencies.

### Minimise Deferred Data
[minimise-deferred-data]: #minimise-deferred-data

Keep the data dependencies and checks as simple as possible.

For example, Zebra could defer checking both the BlockHeight and Network Upgrade.

But since the Network Upgrade depends on the BlockHeight, we only need to defer
the BlockHeight check. Then we can use all the fields that depend on the
BlockHeight, as if it is correct. If the final BlockHeight check fails, we will
reject the entire block, including all the verification we perfomed using the
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

Verification is implemented by the following traits and services:
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
  * Zebra implements the block height consensus rule, which makes sure
    each block is added to state after its previous block.

### Block Verification
[block-verification]: #block-verification

The `BlockVerifier` performs detailed verification of recent blocks, in parallel.

Here is how the `BlockVerifier` implements each verification stage:

* **Structural Verification:**
  * *As Above:* the `BlockVerifier` accepts parsed `Block` structs.
* **Semantic Verification:**
  * *As Above:* verifies each field in the block. Defers any data dependencies as
    long as possible, awaits those data dependencies, then performs data
    dependent checks.
  * Note: Since futures are executed concurrently, we can use the same function
    to:
    * perform context-free verification,
    * perform verification with deferred data dependencies,
    * await data dependencies, and
    * check data dependencies.
    To maximise concurrency, we should write verification functions in this
    specific order, so the awaits are as late as possible.
* **State Updates:**
  * *As Above:* the `BlockVerifier` returns success to the `ChainVerifier`,
    which sends verified `Block`s to the state service.
  * Zebra implements the block height consensus rule, which makes sure
    each block is added to state after its previous block.

## Zcash Protocol Design
[zcash-protocol]: #zcash-protocol

When designing a change to the Zcash protocol, minimise the data dependencies
between blocks.

Try to create designs that:
* Eliminate data dependencies,
* Make the changes depend on a version field in the block header or transaction,
* Make the changes depend on the current Network Upgrade,
* Make the changes depend on a field in the current block, with an additional
  consensus rule to check that field against previous blocks, or
* Prefer dependencies on older blocks, rather than newer blocks
  (older blocks are more likely to verify earlier).

Older dependencies have a design tradeoff:
* depending on multiple blocks is complex,
* depending on the previous block makes parallel verification harder.

When making decisions about this dependency tradeoff, consider:
* how the data dependency could be deferred, and
* the CPU cost of the verification - if it is trivial, then it does not matter if
  the verification is parallelised.

# Drawbacks
[drawbacks]: #drawbacks

This design is a bit complicated, but we think it's necessary to achieve our
goals.

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
