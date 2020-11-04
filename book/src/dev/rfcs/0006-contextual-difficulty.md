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

**median timespan**: The average number of seconds taken to generate the blocks
  in the averaging window. Calculated using the difference of median block spans
  in and after the averaging window, then damped and bounded.

**target timespan**: The target spacing for an averaging window's worth of
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
that there will be at least 28 blocks in any relevant chain.

Difficulty threshold calculations are performed using unsigned 256-bit integers.
Time calculations are performed using unsigned 32-bit integers.

TODO:
  - check how zcashd implements signed median time differences
  - open a ticket to update the Zcash spec

## The relevant chain
[relevant-chain]: #relevant-chain

The relevant chain can be retrieved from the state service [RFC5] as follows:
* if the previous block is the finalized tip:
  * get recent blocks from the finalized state
* if the previous block is in the non-finalized state:
  * get recent blocks from the relevant chain, then
  * get recent blocks from the finalized state, if required

The relevant chain can start at any non-finalized block. If the next block is
valid, it becomes the new tip of the relevant chain.

In particular, if the previous block is not a chain tip, the relevant chain
becomes a new chain fork.

[RFC5]: ./0005-state-updates.md

## Mean target difficulty
[mean-target-difficulty]: #mean-target-difficulty

The mean target difficulty is the arithmetic mean of the difficulty
thresholds of the 17 most recent blocks in the relevant chain.

Zcash uses block difficulty thresholds in its difficulty adjustment calculations.
(Block hashes are not used for difficulty adjustment.)

TODO:
  - check if zcashd truncates the MeanTarget before dividing by AveragingWindowTimespan,
    (as well as after the division)
  - open a ticket to update the Zcash spec

## Median timespan
[median-timespan]: #median-timespan

The average number of seconds taken to generate the 17 blocks in the averaging
window.

Calculated using the difference of the median timespans for:
* the relevant tip: the 11 most recent blocks, and
* the 11 blocks after the 17-block averaging window: blocks 18-28 behind the
  relevant tip.

(The median timespan is known as the `ActualTimespan` in the Zcash specification,
but this terminology is confusing, because it is a difference of medians, rather
than an "actual" elapsed time.)

TODO: open a Zcash spec clarification ticket

The median timespan is damped by the `PoWDampingFactor`, and bounded by
`PoWMaxAdjustDown` and `PoWMaxAdjustUp`.

## Block difficulty threshold
[block-difficulty-threshold]: #block-difficulty-threshold

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the target
timespan.

The block difficulty is limited by the `PoWLimit`, a per-network easiest block
difficulty.

## Test network minimum difficulty blocks
[test-net-min-difficulty]: #test-net-min-difficulty

Starting from testnet block 299188, the difficulty filter on testnet is modified
as follows:

A block passes the difficulty filter if:
* the hash difficulty is less than or equal to the difficulty threshold, or
* the time gap from the previous block is at least 6 times the target spacing,
  and the hash difficulty is less than or equal to the testnet `PoWLimit`
  [ZIP-208].

The value of the difficulty threshold in the block header is used to calculate
the difficulty adjustments for subsequent blocks. These calculations remain the
same, even if the relevant chain contains minimum-difficulty blocks.

[ZIP-208]: https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
