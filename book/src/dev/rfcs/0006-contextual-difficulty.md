- Feature Name: contextual_difficulty_validation
- Start Date: 2020-11-02
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#1036](https://github.com/ZcashFoundation/zebra/issues/1036)

# Summary
[summary]: #summary

Zcash nodes use a Proof of Work algorithm to reach consensus on the best chain.
Valid blocks must reach a difficulty threshold, which is adjusted after every
block. The difficulty adjustment calculations depend on the difficulties and
times of recent blocks. So Zebra performs contextual validation [RFC2] of
difficulty adjustments as part of committing blocks to the state.

[RFC2]: ./0002-parallel-verification.md

# Motivation
[motivation]: #motivation

The Zcash block difficulty adjustment is one of the core Zcash consensus rules.
Zebra must implement this consensus rule to make sure that its cached chain
state is consistent with the consensus of Zcash nodes.

Difficulty adjustment is also a significant part of Zcash's security guarantees.
It ensures that the network continues to resist takeover attacks, even as the
number of Zcash miners grows.

Difficulty adjustment also ensures that blocks are regularly spaced, which
allows users to create and finalise transactions with short, consistent delays.
These predictable delays contribute to Zcash's usability.

# Definitions
[definitions]: #definitions

Difficulty:
* **hash difficulty**: An arbitrary ranking of blocks, based on their hashes.
  Defined as the hash of the block, interpreted as a big-endian 256-bit number.
  Numerically smaller difficulties are harder to generate.

* **difficulty threshold**: The easiest valid hash difficulty for a block.
  Numerically lower thresholds are harder to satisfy.

* **difficulty filter**: A block passes the difficulty filter if the hash
  difficulty is less than or equal to the difficulty threshold (based on the
  block's difficulty field). On testnet, if a long time elapses between blocks,
  the difficulty filter also allows minimum-difficulty blocks.

* **block work**: The approximate amount of work required for a miner to generate
  a block hash that passes the difficulty filter. The number of block header
  attempts and the mining time are proportional to the work value. Numerically
  higher work values represent longer processing times.

* **averaging window**: The 17 most recent blocks in the relevant chain.

* **median block span**: The 11 most recent blocks from a chosen tip, typically
  the relevant tip.

* **target spacing**: 150 seconds per block before Blossom activation, 75 seconds
  per block from Blossom activation onwards.

* **mean target difficulty**: The arithmetic mean of the difficulty thresholds
  of the blocks in the averaging window.

* **median timespan**: The average number of seconds taken to generate the blocks
  in the averaging window. Calculated using the difference of median block spans
  in and after the averaging window, then damped and bounded.

* **target timespan**: The target spacing for an averaging window's worth of
  blocks.

Consensus:
* **consensus rule:** A protocol rule which all nodes must apply consistently,
                      so they can converge on the same chain fork.

* **structural/semantic/contextual verification**: as defined in [RFC2].

State:
* **block chain**: A sequence of valid blocks linked by inclusion of the
  previous block hash in the subsequent block. Chains are rooted at the
  genesis block and extend to a tip.

* **relevant chain**: The relevant chain for a block starts at the previous
  block, and extends back to genesis.

* **relevant tip**: The tip of the relevant chain.

* **non-finalized state**: State data corresponding to blocks above the reorg
  limit. This data can change in the event of a chain reorg.

* **finalized state**: State data corresponding to blocks below the reorg
  limit. This data cannot change in the event of a chain reorg.

* **non-finalized tips**: The highest blocks in each non-finalized chain. These
  tips might be at different heights.

* **finalized tip**: The highest block in the finalized state. The tip of the best
  chain is usually 100 blocks (the reorg limit) above the finalized tip. But it can
  be lower during the initial sync, and after a chain reorganization, if the new
  best chain is at a lower height.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

The difficulty threshold for the next block is calculated using the difficulty
thresholds and times of recent blocks. Zcash uses the most recent 28 blocks in
the **relevant chain** in its difficulty adjustment calculations.

The difficulty adjustment calculations adjust the **mean target difficulty**,
based on the difference between the **median timespan** and the
**target timespan**.

Since contextual validation is only used for post-Sapling blocks, we can assume
that there will be at least 28 blocks in any relevant chain on mainnet and
testnet.

Difficulty threshold calculations are performed using unsigned 256-bit integers.
In the Zcash specification, time values are 32-bit integers. But the difficulty
adjustment calculations include time subtractions which could overflow an
unsigned type, so they are performed using signed 64-bit integers in `zcashd`.

Zebra is free to implement its calculations in any way that produces equivalent
results. Using `u256` difficulty, `u32` unsigned times, and `i64` signed time
differences will allow us to write simple Rust code which produces the correct
results. (It is theoretically possible for the time gap between blocks to be
larger than `2^31 - 1`, because those times are provided by miners. Even if the
median time gap is that large, the bounds and minimum difficulty in Zcash's
difficulty adjustment algorithm will preserve a reasonable difficulty
threshold.)

## State service interface changes
[state-service-interface]: #state-service-interface

Contextual validation accesses recent blocks. So we modify the internal state
service interface to provide an abstraction for accessing recent blocks.

### The relevant chain
[relevant-chain]: #relevant-chain

The relevant chain can be retrieved from the state service [RFC5] as follows:
* if the previous block is the finalized tip:
  * get recent blocks from the finalized state
* if the previous block is in the non-finalized state:
  * get recent blocks from the relevant chain, then
  * get recent blocks from the finalized state, if required

The relevant chain can start at any non-finalized block, or at the finalized tip.
If the next block is valid, it becomes the new tip of the relevant chain.

In particular, if the previous block is not a chain tip, the relevant chain
becomes a new chain fork.

[RFC5]: ./0005-state-updates.md

## Contextual validation design
[contextual-validation-design]: #contextual-validation-design

Contextual validation is performed synchronously by the state service, as soon
as the state has:
* received the semantically valid next block (via `CommitBlock`), and
* committed the previous block.

Contextual difficulty validation consists of the following check:
* the difficulty threshold in the block matches the adjusted difficulty from
  previous blocks.

This check is implemented as follows:

### Difficulty adjustment
[difficulty-adjustment]: #difficulty-adjustment

The block difficulty threshold is adjusted by scaling the mean target difficulty
by the median timespan.

On testnet, if a long time has elapsed since the previous block, the difficulty
adjustment is modified to allow minimum-difficulty blocks.

#### Mean target difficulty
[mean-target-difficulty]: #mean-target-difficulty

The mean target difficulty is the arithmetic mean of the difficulty
thresholds of the `PoWAveragingWindow` (17) most recent blocks in the relevant
chain.

Zcash uses block difficulty thresholds in its difficulty adjustment calculations.
(Block hashes are not used for difficulty adjustment.)

Note that `zcashd` truncates the `MeanTarget` after the mean calculation, and
after dividing by `AveragingWindowTimespan`. But as long as there is no overflow,
this is [equivalent to the single truncation of the final result] in the Zcash
specification. However, Zebra should follow the order of operations in `zcashd`,
because repeated divisions can't overflow.

[equivalent to the single truncation of the final result]: https://math.stackexchange.com/questions/147771/rewriting-repeated-integer-division-with-multiplication

#### Median timespan
[median-timespan]: #median-timespan

The average number of seconds taken to generate the 17 blocks in the averaging
window.

Calculated using the difference of the median timespans for:
* the relevant tip: the `PoWMedianBlockSpan` (11) most recent blocks, and
* the 11 blocks after the 17-block averaging window: that is, blocks 18-28 behind
  the relevant tip.

(The median timespan is known as the `ActualTimespan` in the Zcash specification,
but this terminology is confusing, because it is a difference of medians, rather
than any "actual" elapsed time.)

The median timespan is damped by the `PoWDampingFactor`, and bounded by
`PoWMaxAdjustDown` and `PoWMaxAdjustUp`.

#### Block difficulty threshold
[block-difficulty-threshold]: #block-difficulty-threshold

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the target
timespan.

The block difficulty is also limited by `ToCompact(PoWLimit(network))`, a
per-network easiest block difficulty.

#### Test network minimum difficulty blocks
[test-net-min-difficulty]: #test-net-min-difficulty

A block is a testnet minimum difficulty block if:
* the block is a testnet block,
* the block's height is 299188 or greater, and
* the time gap from the previous block is greater than the testnet minimum
  difficulty gap, which is 6 times the target spacing for the block's height.

The difficulty adjustment is modified for testnet minimum difficulty blocks as
follows:
* the difficulty threshold in the block header is set to the testnet minimum
  difficulty threshold, `ToCompact(PoWLimit(network))`.

Since the new difficulty changes the block header, testnet blocks can only
satisfy one of the alternate difficulty adjustment rules:
* if the time gap is less than or equal to the testnet minimum difficulty gap:
  the difficulty threshold is calculated using the default difficulty adjustment
  rule,
* if the time gap is greater than the testnet minimum difficulty gap:
  the difficulty threshold is the testnet minimum difficulty threshold.

See [ZIP-208] for details.

Note: There were several errors in the specification of testnet minimum
difficulty adjustment in ZIPs 205 and 208. The time gap, minimum difficulty
threshold value, the modification of the `difficulty` (`nBits`) field, and its
use in future difficulty adjustments were all incorrect. These errors are fixed
in [ZIP PR 417] and [ZIP commit 806076c].

[ZIP-208]: https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
[ZIP PR 417]: https://github.com/zcash/zips/pull/417
[ZIP commit 806076c]: https://github.com/zcash/zips/commit/806076c48c9834fd9941b940a32310d737975a3a

# ----- TODO: Write the rest of the design -----

Explain the proposal as if it was already included in the project and you were teaching it to another Zebra programmer. That generally means:

- [x] Introducing new named concepts.
- [ ] Explaining the feature largely in terms of examples.
- [x] Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra. It should explain the impact as concretely as possible.
- [ ] If applicable, provide sample error messages, deprecation warnings, migration guidance, or test strategies.

For implementation-oriented RFCs (e.g. for Zebra internals), this section should focus on how Zebra contributors should think about the change, and give examples of its concrete impact.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

This is the technical portion of the RFC. Explain the design in sufficient detail that:

- Its interaction with other features is clear.
- It is reasonably clear how the feature would be implemented, tested, monitored, and maintained.
- Corner cases are dissected by example.

The section should return to the examples given in the previous section, and explain more fully how the detailed proposal makes those examples work.

## Module Structure

Describe the crate and modules that will implement the feature.

## Test Plan

Explain how the feature will be tested, including:
* tests for consensus-critical functionality
* existing test vectors, if available
* Zcash blockchain block test vectors (specify the network upgrade, feature, or block height and network)
* property testing or fuzzing

The tests should cover:
* positive cases: make sure the feature accepts valid inputs
  * using block test vectors for each network upgrade provides some coverage of valid inputs
* negative cases: make sure the feature rejects invalid inputs
  * make sure there is a test case for each error condition in the code
  * if there are lots of potential errors, prioritise:
    * consensus-critical errors
    * security-critical errors, and
    * likely errors
* edge cases: make sure that boundary conditions are correctly handled

# Drawbacks
[drawbacks]: #drawbacks

Why should we *not* do this?

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

- What makes this design a good design?
- Is this design a good basis for later designs or implementations?
- What other designs have been considered and what is the rationale for not choosing them?
- What is the impact of not doing this?

# Prior art
[prior-art]: #prior-art

Discuss prior art, both the good and the bad, in relation to this proposal.
A few examples of what this can include are:

- For community proposals: Is this done by some other community and what were their experiences with it?
- For other teams: What lessons can we learn from what other communities have done here?
- Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background.

This section is intended to encourage you as an author to think about the lessons from other projects, to provide readers of your RFC with a fuller picture.
If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if they are an adaptation from other projects.

Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC.
Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features and designs.

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- What parts of the design do you expect to resolve through the RFC process before this gets merged?
- What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
- What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?

# Future possibilities
[future-possibilities]: #future-possibilities

Think about what the natural extension and evolution of your proposal would
be and how it would affect Zebra and Zcash as a whole. Try to use this
section as a tool to more fully consider all possible
interactions with the project and cryptocurrency ecosystem in your proposal.
Also consider how the this all fits into the roadmap for the project
and of the relevant sub-team.

This is also a good place to "dump ideas", if they are out of scope for the
RFC you are writing but otherwise related.

If you have tried and cannot think of any future possibilities,
you may simply state that you cannot think of anything.

Note that having something written down in the future-possibilities section
is not a reason to accept the current or a future RFC; such notes should be
in the section on motivation or rationale in this or subsequent RFCs.
The section merely provides additional information.
