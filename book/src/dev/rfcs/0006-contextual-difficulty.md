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
* **block difficulty**: An arbitrary ranking of blocks, based on their hashes.
  Defined as the hash of the block, interpreted as a big-endian 256-bit number.
  Numerically smaller difficulties are harder to generate.

* **difficulty threshold**: The easiest valid **block difficulty** for a
  block. Numerically lower thresholds are harder to satisfy.

* **block work**: The approximate amount of work required for a miner to generate
  a block that satisfies a **difficulty threshold**. The number of block header
  attempts and the mining time are proportional to the work value. Numerically
  higher work values are harder to satisfy.

* **cumulative work**: The sum of the **block work** of all blocks in a chain,
  from genesis to the chain tip.

Consensus:
* **consensus rule:** A protocol rule which all nodes must apply consistently,
                      so they can converge on the same chain fork.

* **structural/semantic/contextual verification**: as defined in [RFC2].

State:
* **block chain**: A sequence of valid blocks linked by inclusion of the
  previous block hash in the subsequent block. Chains are rooted at the
  genesis block and extend to a tip.

* **best chain**: The chain with the greatest **cumulative work**. This chain
  represents the consensus state of the Zcash network and transactions.

* **relevant chain**: The relevant chain for a block starts at the previous
  block, and extends back to genesis.

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
