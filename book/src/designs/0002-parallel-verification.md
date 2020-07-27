# Parallel Verification

- Feature Name: parallel_verification
- Start Date: 2020-07-27
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#682](https://github.com/ZcashFoundation/zebra/issues/682)

# Summary
[summary]: #summary

Zebra verifies blocks in several stages, most of which can be executed in
parallel.

We use several different design patterns to enable this parallelism:
* We defer data dependencies by assuming that unknown inputs are correct, and
  then checking those assumptions in a later verification stage.
* We verify new blocks added to each recent chain tip in parallel, using a
  context based on that chain's previous blocks.
* We hold recent chains in memory, and defer writing changes to disk until they
  are past the chain reorganisation limit.
* We use immutable shared data structures and deltas to limit the memory used
  by chain contexts.

# Motivation
[motivation]: #motivation

Zcash (and Bitcoin) are designed to verify each block in sequence, starting
from the genesis block. But during the initial sync, and when restarting with
an older state, this process can be quite slow.

By parallelising block and transaction verification, we can use multithreading
and batch verification for signatures, proofs, scripts, and hashes. By
parallelising chain tip verification, and keeping recent chains in memory, we
remove the need for rollbacks, and make chain tip updates into an atomic
operation. By using an immutable main chain context to update the disk state,
we correctly update the disk state, even if the main chain tip changes during
the update.

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

In Zebra, verification happens in the following stages:
* **Structural Verification:** Raw block data is parsed into a block header and
  transactions. Invalid data is not representable in these structures:
  deserialization (parsing) can fail, but serialization always succeeds.
* **Context-Free Verification:** Fields that don't depend on any context from
  previous blocks are verified.
* **Prospective Verification:** Some fields depend on context which can be
  derived, without access to data from previous blocks. These fields are
  verified, assuming that their context is correct. These assumptions are
  passed to the next verification stage as a set of constraints.
* **Contextual Verification:** Constraints from the previous stage are checked
  against the context associated with the previous block and its ancestors (the
  "chain context"). The remaining fields that depend on the chain context are
  also verified at this stage.
* **In-Memory Chain Updates:** An updated chain context is created for each
  block, based on the parent block's chain context.
* **Chain Tip Updates:** The set of chain tips is updated as each block is
  verified. The main chain tip is updated based on the consensus rules.
* **Main Chain Disk Updates:** When a main chain block is behind the main
  chain's tip by more than the reorganisation limit (100 blocks), it is stored
  to disk. The on-disk state is updated based on that block's chain context.
  The in-memory block and chain context are dropped to reclaim resources.
* **Chain Pruning:** When a chain is behind the main chain's tip by more than
  the pruning limit (100 blocks in Zebra, 288 blocks in zcashd), its in-memory
  blocks and chain contexts are dropped to reclaim resources.

This RFC focuses on the verification stages that use chain context, and the
design patterns that enable them to be excuted in parallel.

## Verification Interfaces
[verification-interfaces]: #verification-interfaces

Verifcation is implemented by the following traits and services:
* **Structural Verification:**
  * `zebra_chain::ZcashDeserialize`: A trait for parsing consensus-critical
    data structures from a byte buffer.
  * `zebra_network::init`: Provides a downloader service that accepts a
    `BlockHeaderHash` request, and parses the peer response into a `Block`
    struct.
* **Context-Free Verification:**
  * `zebra_consensus::ContextFreeVerifier`: Provides a verifier service that
    accepts a `Block` request, performs context-free verification on the block,
    and responds with an identifier for the block.
  * Note: as of 27 July 2020, context-free verification is partly implemented
    by `BlockVerifier`, which responds with `BlockHeaderHash`.
* **Prospective Verification:**
  * `zebra_consensus::ProspectiveVerifier`: Provides a verifier service that
    accepts a `Block` request, performs prospective verification on the block,
    and responds with the block's `VerificationConstraints`, which include a
    reference to the `Block` itself.
* **Constraint Verification:**
  * `zebra_consensus::ConstraintVerifier`: Provides a verifier service that
    accepts a `VerificationConstraints, previous_context: ChainContext`
    request, performs constraint checks on the `Block` and `ChainContext`, and
    responds with an idenfier for the block.
  * Note: blocks can be identified by their BlockHeaderHash, or by reference.
* **Contextual Verification:**
  * `zebra_consensus::ContextualVerifier`: Provides a verifier service that
    accepts a `Block, previous_context: ChainContext` request, performs any
    remaining contextual verification on the block, and responds with an
    identifier for the block.
* **In-Memory Chain Updates:**
  * `zebra_consensus::RecentChainUpdater`: Provides an updater service that
    accepts a `Block, previous_context: ChainContext` request, generates an
    updated chain context, dropping references to any ancestor blocks that are
    no longer required to verify subsequent blocks. The service responds with
    an updated `ChainContext`, which contains a reference to the block itself.
* **Chain Tip Updates:**
  * `zebra_consensus::ChainTipUpdater`: Provides an updater service that
    accepts a `ChainContext, Arc<Mutex<ChainTips>>>` request, updates
    the set of chain tips, and updates the main chain tip, if required.
    (New blocks on side-chains might not lead to a main chain tip update.)
    The `ChainContext` becomes one of the new tips in the list.
    The service responds with the updated `Arc<Mutex<ChainTips>>`.
  * Note: The service requires exclusive write access to the chain tips, so it
    can atomically update the main tip and the set of chain tips.
* **Main Chain Disk Updates:**
  * `zebra_consensus::MainChainDiskUpdater`: Provides an updater service that
    accepts an `Arc<Mutex<ChainTips>>, Transaction<DiskState>` request. This
    service updates the disk state based on the earliest chain contexts in the
    main chain, and their associated blocks.
  * Note: If there is a chain reorganisation, there may be zero or many
    contexts that are past the reorg limit.
  * Note: The service requires exclusive write access to the disk state, and
    shared read access to the chain tips, so that:
    * it has a consistent view of the chain tips and main tip, and
    * other parts of the application have a consistent view of the disk state
      and chain contexts. For example, the set of in-memory deltas needs to be
      consistent with the disk state.
* **Chain Pruning:**
  * `zebra_state::ChainPruner`: Provides a pruning service that accepts an
    `Arc<Mutex<ChainTips>>` request, removes any orphaned side-chains from the
    set of tips, and drops the earliest chain contexts in all chains.
  * Note: If there is a chain reorganisation, there may be zero or many
    contexts that are past the reorg limit.
  * Note: To reclaim resources, the chain pruner needs sole ownership of the
    contexts that are past the reorg limit. If it does not have sole
    ownership, this might indicate a bug in Zebra. If the service encounters
    this bug, it should warn about potential memory leaks.
  * Note: The service requires exclusive write access to the chain tips, so it
    can atomically update the main tip and the set of chain tips.

### Checkpoint Verification
[checkpoint-verification]: #checkpoint-verification

The `CheckpointVerifier` performs rapid verification of blocks, based on
a set of hard-coded checkpoints. Each checkpoint hash can be used to verify
all the previous blocks, back to the genesis block. So Zebra can skip almost
all context-free and contextual verification for blocks in the checkpoint
range.

The `CheckpointVerifier` uses an internal queue to implement its own chain
context. Checkpoint verification is cheap, so it is implemented using
non-async functions within the CheckpointVerifier service.

Here is how the `CheckpointVerifier` implements each verification stage:

* **Structural Verification:**
  * *As Above:* the `CheckpointVerifier` accepts parsed `Block` structs.
* **Context-Free Verification:**
  * `check_height`: makes sure the block height is within the unverified
    checkpoint range, and adds the block to its internal queue.
* **Prospective Verification:**
  * `target_checkpoint_height`: Checks for a continuous range of blocks from
    the previous checkpoint to a subsequent checkpoint. If the chain is
    incomplete, returns a future, and waits for more blocks. If the chain is
    complete, assumes that the `previous_block_hash` fields of these blocks
    form an unbroken chain from checkpoint to checkpoint, and starts
    processing the checkpoint range. (This constraint is an implicit part of
    the `CheckpointVerifier` design.)
* **Constraint Verification:**
  * `process_checkpoint_range`: makes sure that the blocks in the checkpoint
    range have an unbroken chain of previous block hashes.
* **Contextual Verification:**
  * *Not Required:* Verifying a chain of blocks against its checkpoints
    confirms that the network considers those blocks valid. (Strictly, that the
    network considered those blocks valid, up to and including the time when
    those checkpoints were created.)
* **In-Memory Chain Updates:**
  * The checkpoint verifier uses an internal queue of blocks to store the
    simple height and hash context it requires for verification.
  * *As Above*: Although the checkpoint verifier does not require any external
    context, Zebra needs to maintain enough context to verify the first
    non-checkpoint block.
  * Since there is only ever a single checkpoint chain, Zebra does not need to
    keep any previous contexts, until it is processing the last 100
    checkpoint blocks.
  * Note: If any context fields are only used to verify blocks within the
    checkpoint range, then Zebra does not need to keep that context. (For
    example, sprout-only context.)
* **Chain Tip Updates:**
  * *Not Required:* Since there is only a single chain, the main chain tip is
    the unique tip. As each checkpoint is verified, it implicitly becomes the
    main tip.
* **Main Chain Disk Updates:**
  * *As Above*: Any large context that is required to verify the first
    non-checkpoint block needs to be stored to disk.
* **Chain Pruning:**
  * *Not Required:* Since Zebra does not keep previous chain contexts until
    the last 100 checkpoint blocks, it will never need to prune any old
    contexts. The earliest checkpoint context will be pruned after the first
    non-checkpoint block is verified. (Since there is only one checkpoint
    chain, there are no side-chains to prune.)

**TODO:**
  - Describe network upgrades, and the ChainContext edge case for the
    activation block
  - Describe the genesis block RecentChainUpdater and MainChainUpdater edge
    cases
  - Describe deltas and large on-disk state
  - Describe how atomic chain tip updates work

# Drawbacks
[drawbacks]: #drawbacks

> Why should we *not* do this?

**TODO:** well, it's a bit complicated, isn't it?

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

**TODO:** expand on notes below

- Why is this design the best in the space of possible designs?
  - Right now we'd probably settle for functional and workable designs
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
    - the shorter pruning limit
    - keeping reorgs in memory
  - tower

> Discuss prior art, both the good and the bad, in relation to this proposal.
> A few examples of what this can include are:
>
> - For other teams: What lessons can we learn from what other communities have done here?
> - Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background.
>
> This section is intended to encourage you as an author to think about the lessons from other languages, provide readers of your RFC with a fuller picture.
> If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if it is an adaptation from other languages.
>
> Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC.
> Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features.

# Unresolved questions
[unresolved-questions]: #unresolved-questions


**TODO:**

General
  - Work out what we need to decide before implementation
  - Describe scope

Specific
  - What does zebra-state need to know to maintain the on-disk and in-memory
    state? What does its interface look like?
  - How many blocks do we need to keep as part of each context? Does it vary by
    network upgrade?
  - Assign each field verification to a stage, based on its data dependencies
    - For example: block header fields, transaction fields
  - Describe the fields in the ChainContext and DiskState, and how they are updated

> - What parts of the design do you expect to resolve through the RFC process before this gets merged?
> - What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
> - What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?

# Future possibilities
[future-possibilities]: #future-possibilities

**TODO:**
  - Put out of scope or unrelated stuff here

> Think about what the natural extension and evolution of your proposal would
be and how it would affect the language and project as a whole in a holistic
way. Try to use this section as a tool to more fully consider all possible
interactions with the project and language in your proposal.
> Also consider how the this all fits into the roadmap for the project
and of the relevant sub-team.
>
> This is also a good place to "dump ideas", if they are out of scope for the
RFC you are writing but otherwise related.
>
> If you have tried and cannot think of any future possibilities,
you may simply state that you cannot think of anything.
>
> Note that having something written down in the future-possibilities section
is not a reason to accept the current or a future RFC; such notes should be
in the section on motivation or rationale in this or subsequent RFCs.
> The section merely provides additional information.
