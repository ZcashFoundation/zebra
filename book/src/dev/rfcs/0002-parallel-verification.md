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

## Chain Context
[chain-context]: #chain-context

The `ChainContext` for a block contains the contextual information needed to
verify the next block in the chain.

There are a few exceptions to this general principle:
* If the context is a field in recent block headers, it may be retrieved via
  the list of recent blocks in the chain context.
* Large state, such as unspent transaction outputs (UTXOs), is stored on disk.
  The chain context stores deltas from recent blocks, which can be used to
  avoid some disk state queries.

In the event of a chain fork, there may be multiple next blocks based on the
current block, and multiple descendant chains. The chain contexts in forks are
different, based on the different blocks in each fork. But they are based on
the same pre-fork chain context.

### Chain Context and Network Upgrades
[chain-context-network-upgrades]: #chain-context-network-upgrades

Each Network Upgrade includes some bilateral consensus rule changes. These
consensus rule changes may affect the `ChainContext` required to verify
post-activation blocks.

For most blocks, the chain updaters create a context with the same fields as
the previous context. Network ugrade activation blocks are an exception, so
they require special handling.

Here are the context changes required for each network upgrade:
* **Genesis:**
  * "the genesis block is the first block to be committed to the state" is the
    only constraint in `VerificationConstraints`

  * There is no `previous_context`, and the `DiskState` is empty.
  * The Genesis RecentChainUpdater creates a new, empty context, using the
    specified initial values for each field.
  * Then the context is updated using the values from the Genesis block,
    modified by any special Genesis rules.
  * The `DiskState` needs to be initialised with the specified initial values.
  * there are no `ChainTips` in memory or on disk - the genesis block
    automatically becomes the first tip
  * Note:
    * the UTXO from the genesis coinbase transaction is not included in the
      UTXO set. (zcashd inheritied this bug from Bitcoin.)
    * the Founders Reward is not required in genesis blocks.
    * the `hashPrevBlock`, difficulty adjustment, and median-time-past rules
      are special-cased.
  * *TODO: check for other differences*  
* **Before Overwinter:**
  * Add UTXOs, note commitments, and nullifiers from each block to the
    `ChainContext` as deltas.
  * Store the full sets in the `DiskState`, and apply deltas to update them.
* **Overwinter:**
  * Additional context required to verify Overwinter (v3) transactions, if any.
* **Sapling:**
  * Additional context required to verify Sapling (v4) transactions, if any.
* **Blossom:**
  * If the context contains pre-calculated fields that depend on
    `AveragingWindowTimespan`, re-calculate those fields. See
    [ZIP-208](https://zips.z.cash/zip-0208#effect-on-difficulty-adjustment)
    for details.
  * Note: Zebra can avoid this issue by always verifying the `bits`
    (difficulty) and `time` fields based on recent block headers, without
    storing any intermediate values in the context.
* **Heartwood:**
  * Heartwood changes the meaning of the `history_root_hash` field in the block
    header. For Sapling and Blossom, it is the final sapling treestate hash.
    For Heartwood, it is the root hash of a Merkle Mountain Range (MMR) which
    commits to various features of the chain history. See
    [ZIP-221](https://zips.z.cash/zip-0221) for details.
  * Each new block adds a leaf node, then merges subtree roots, if possible.
    It generates some new "extra nodes" to bag the subtrees, then calculates
    the root hash.
  * To efficiently verify this field, Zebra can store the list of MMR previous
    subtree roots in the `ChainContext` as an `im::Vector`. The number of
    subtree roots is at most `log(height - activation_height)`.
  * As an optimisation, we could also store a list of recent "extra nodes"
    generated during the bagging process.
  * Store the full list of previous subtree roots in the `DiskState`. Update by
    replacing the old `DiskState` list with the new `ChainContext` set.
  * The Heartwood activation block has an all-zeroes `history_root_hash` field,
    and an empty list of previous subtree roots.

## Main Chain Tip
[main-chain-tip]: #main-chain-tip

The main chain tip determines the set of valid, historical Zcash transactions.
The main chain tip is updated whenever a new block is received:

First, the set of chain tips is updated:
* if the new block uniquely extends an existing chain tip, that existing chain
  tip is replaced by the new block.
* if the new block forks from the ancestor of an existing tip, the new block
  is added to the set of chain tips.

Then, the new main tip is selected, according to these rules:
* the main tip is the chain tip with the greatest cumulative proof of work,
  calculated according to the Zcash Specification. (This is a consensus rule.)
* as a tie-breaker, if multiple chain tips have equal cumulative work, and one
  of those tips is the current main tip, the main tip does not change. This
  check can be implemented using a strictly greater than comparison. (This
  is *not* a consensus rule, because it depends on download and
  verification order on each local node. But it is an important feature of node
  implementations, because it helps the network converge on a single
  chain.)
  * Note: zcashd chooses the first block that was downloaded on the local
    node. But in Zebra, we want to avoid tracking an associated download time
    for each block (in memory and on disk).
* as a tie-breaker, if the main tip is not one of the chain tips with the
  greatest cumulative work, the service chooses an arbitrary chain tip.
  (This is *not* a consensus rule, and it should not affect network
  convergence, because that is handled by the previous two rules.)
   * Note: we can avoid this edge case by making sure that all verified
     blocks are associated with a chain. This ensures they are an
     ancestor of at least one tip. (Or the tip itself.)
  * Note: Since the service has exclusive access to the chain tips, and
    it only adds one block at a time, this edge case should be
    impossible.
  * Note: if a network upgrade changes the proof of work rules, it
     could cause a tie. We should review this design if the proof of
     work rules change.

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
  - zcashd divergence
    - verification time as a main chain tip tie-breaker
    - a shorter pruning limit
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
