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

## State service interface changes
[state-service-interface]: #state-service-interface

Contextual validation accesses recent blocks. So we modify the internal state
service interface to provide an abstraction for accessing recent blocks.

### The relevant chain
[relevant-chain]: #relevant-chain

The relevant chain consists of the ancestors of a block, starting with its
parent block, and extending back to the genesis block.

In Zebra, recent blocks are part of the non-finalized state, which can contain
multiple chains. Past the reorganization limit, Zebra commits a single chain to
the finalized state.

The relevant chain can start at any block in the non-finalized state, or at the
finalized tip.

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

#### Median timespan
[median-timespan]: #median-timespan

The average number of seconds taken to generate the 17 blocks in the averaging
window.

The median timespan is calculated by taking the difference of the median times
for:
* the relevant tip: the `PoWMedianBlockSpan` (11) most recent blocks, and
* the 11 blocks after the 17-block `PoWAveragingWindow`: that is, blocks 18-28
  behind the relevant tip.

The median timespan is damped by the `PoWDampingFactor`, and bounded by
`PoWMaxAdjustDown` and `PoWMaxAdjustUp`.

#### Test network minimum difficulty blocks
[test-net-min-difficulty]: #test-net-min-difficulty

If there is a large gap after a testnet block, the next block becomes a minimum
difficulty block. Testnet minimum difficulty blocks have their
`difficulty_threshold` set to the minimum difficulty for testnet.

#### Block difficulty threshold
[block-difficulty-threshold]: #block-difficulty-threshold

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the averaging
window timespan.

The result of this calculation is limited by `ToCompact(PoWLimit(network))`, a
per-network minimum block difficulty. This minimum difficulty is also used when
a testnet block's time gap exceeds the minimum difficulty gap.

## Remaining TODOs for Guide-level explanation

- [x] Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra.
  - [ ] It should explain the impact as concretely as possible.
- [ ] Explaining the feature largely in terms of examples.
- [ ] If applicable, provide sample error messages, deprecation warnings, migration guidance, or test strategies.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Contextual validation
[contextual-validation]: #contextual-validation

Contextual validation is implemented in
`StateService::check_contextual_validity`, which calls a separate function for
each contextual validity check.

In Zebra, contextual validation starts after Sapling activation, so we can assume
that the relevant chain contains at least 28 blocks on Mainnet and Testnet. (And
panic if this assumption does not hold at runtime.)

For debugging purposes, the candidate block's height, hash, and network should be
included in a span that is active for the entire contextual validation function.

## Data types
[data-types]: #data-types

Zebra is free to implement its difficulty calculations in any way that produces
equivalent results to `zcashd` and the Zcash specification.

In Zcash, difficulty threshold calculations are performed using unsigned 256-bit
integers. Rust has no standard `u256` type, but there are a number of crates
available which implement the required operations on 256-bit integers.

In Zcash, time values are 32-bit integers. But the difficulty adjustment
calculations include time subtractions which could overflow an unsigned type, so
they are performed using signed 64-bit integers in `zcashd`.

Zebra parses the `header.time` field into a `DateTime<Utc>`. Conveniently, the
`chrono::DateTime<_>::timestamp()` function returns `i64` values. So Zebra can do
its signed time calculations using `i64` values.

Note: `i32` is an unsuitable type for signed time calculations. It is
theoretically possible for the time gap between blocks to be larger than
`2^31 - 1`, because those times are provided by miners. Even if the median time
gap is that large, the bounds and minimum difficulty in Zcash's difficulty
adjustment algorithm will preserve a reasonable difficulty threshold. So Zebra
must support this edge case.

## Relevant chain iterator
[relevant-chain-iterator]: #relevant-chain-iterator

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

### Relevant chain implementation
[relevant-chain-implementation]: #relevant-chain-implementation

The relevant chain is implemented as a `StateService` iterator, which returns
`Arc<Block>`s.

The chain iterator implements `ExactSizeIterator`, so Zebra can efficiently
assert that the relevant chain is at least 28 blocks long, before starting
contextual validation.

```rust
    /// Return an iterator over the relevant chain of the block identified by
    /// `hash`.
    ///
    /// The block identified by `hash` is included in the chain of blocks yielded
    /// by the iterator.
    pub fn chain(&self, hash: block::Hash) -> Iter<'_> { ... }

    impl Iterator for Iter<'_>  { ... }
    impl ExactSizeIterator for Iter<'_> { ... }
    impl FusedIterator for Iter<'_> {}
```

For further details, see [PR 1271].

[PR 1271]: https://github.com/ZcashFoundation/zebra/pull/1271

## Difficulty adjustment check
[difficulty-adjustment-check]: #difficulty-adjustment-check

The difficulty adjustment check calculates the correct difficulty threshold
value for a candidate block, and ensures that the block's
`difficulty_threshold` field is equal to that value.

The difficulty adjustment check is implemented as a function which takes a
candidate block's `difficulty_threshold`, `height`, and `network`, and a
`context`. The context is a slice of 28 block `(time, difficulty_threshold)`
pairs from the relevant chain.

We avoid passing the entire `Block` and `Iter` to the validation function for
a few reasons:
* using specific types makes it easier to call the function with the correct data,
* limiting the arguments to required data makes it easier to test the function,
  and
* in the future, we want to validate difficulty adjustment as part of gossiped
  header verification, when we haven't yet downloaded the full block
  (see [Issue 1166]).

```rust
/// Validate the `difficulty_threshold` from a candidate block, based on that
/// block's `time`, `network` and `height`, and some `context` data from recent
/// blocks.
///
/// The `context` contains the `difficulty_threshold`s and `time`s from the
/// previous `PoWAveragingWindow + PoWMedianBlockSpan` blocks in the relevant
/// chain, in reverse height order, starting with the parent block.
pub fn difficulty_threshold_is_valid(difficulty_threshold: CompactDifficulty,
                                     time: DateTime<Utc>,
                                     network: Network,
                                     height: block::Height,
                                     context: &[(CompactDifficulty, DateTime<Utc>); 28])
                                     -> Result<(), BlockError> { ... }
```

`difficulty_threshold_is_valid` is located in the existing
`zebra_consensus::block::check` module.

[Issue 1166]: https://github.com/ZcashFoundation/zebra/issues/1166

### Mean target difficulty calculation
[mean-target-difficulty-calculation]: #mean-target-difficulty-calculation

The mean target difficulty is the arithmetic mean of the difficulty
thresholds of the `PoWAveragingWindow` (17) most recent blocks in the relevant
chain.

Since the `PoWLimit`s are `2^251 − 1` for Testnet, and `2^243 − 1` for mainnet,
the sum of these difficulty thresholds will be less than or equal to
`(2^251 − 1)*17 = 2^255 + 2^251 - 17`. Therefore, this calculation can not
overflow a `u256` value.

In Zebra, contextual validation starts after Sapling activation, so we can assume
that the relevant chain contains at least 17 blocks. Therefore, the `PoWLimit`
case of `MeanTarget()` in the Zcash specification is unreachable.

```rust
/// Calculate the arithmetic mean of `averaging_window_thresholds`: the
/// `difficulty_threshold`s from the previous `PoWAveragingWindow` blocks in the
/// relevant chain.
///
/// Implements `MeanTarget` from the Zcash specification.
fn mean_target_difficulty(averaging_window_thresholds: &[ExpandedDifficulty; 17])
                          -> ExpandedDifficulty { ... }
```

`mean_target_difficulty` is located in the existing
`zebra_consensus::work::difficulty` module.

### Median timespan calculation
[median-timespan-calculation]: #median-timespan-calculation

The median timespan is the difference of the median times for:
* the relevant tip: the `PoWMedianBlockSpan` (11) most recent blocks, and
* the 11 blocks after the 17-block `PoWAveragingWindow`: that is, blocks 18-28
  behind the relevant tip.

(The median timespan is known as the `ActualTimespan` in the Zcash specification,
but this terminology is confusing, because it is a difference of medians, rather
than any "actual" elapsed time.)

In Zebra, contextual validation starts after Sapling activation, so we can assume
that the relevant chain contains at least 28 blocks. Therefore:
* `max(0, height − PoWMedianBlockSpan)` in the `MedianTime()` calculation
   simplifies to `height − PoWMedianBlockSpan`,
* there is always an odd number of blocks in `MedianTime()`, so the median is
  always the exact middle of the sequence,
* we only need the candidate block's network upgrade to determine the
  `AveragingWindowTimespan`, so we don't need the block's network, and
* we don't need to know the block's height, because all the other uses of
  `height` in the Zcash specification are implicitly handled by indexing into
  the `timespan_times` slice.

Zebra calculates the median timespan using the following functions:
```rust
/// Calculate the damped and bounded median of `timespan_times`: the `time`s
/// from the previous `PoWAveragingWindow + PoWMedianBlockSpan` blocks in the
/// relevant chain. Uses the candidate block's `height' and `network` to
/// calculate the `AveragingWindowTimespan` for that block.
///
/// The times in `timespan_times` must be supplied in reverse height order,
/// starting with the parent block. This might not be the same as chronological
/// order, because block times are supplied by miners.
///
/// The median timespan is damped by the `PoWDampingFactor`, and bounded by
/// `PoWMaxAdjustDown` and `PoWMaxAdjustUp`.
///
/// Implements `ActualTimespanBounded` from the Zcash specification.
///
/// Note: This calculation only uses a `PoWMedianBlockSpan` of times at the
/// start and end of `timespan_times`. The `timespan_times[11..=16]` in the
/// middle of the slice are ignored.
fn median_timespan_bounded(height: block::Height,
                           network: Network,
                           timespan_times: &[DateTime<Utc>; 28])
                           -> DateTime<Utc> { ... }

/// Calculate the median of `median_block_span_times`: the `time`s from a span of
/// `PoWMedianBlockSpan` blocks in the relevant chain.
///
/// Implements `MedianTime` from the Zcash specification.
fn median_time(median_block_span_times: &[DateTime<Utc>; 11])
               -> DateTime<Utc> { ... }
```

`median_timespan_bounded` and `median_time` are located in the existing
`zebra_consensus::work::difficulty` module.

Zebra calculates the `AveragingWindowTimespan` using the following functions:
```rust
impl NetworkUpgrade {
    /// Returns the `AveragingWindowTimespan` for the network upgrade.
    pub fn averaging_window_timespan(&self) -> Duration { ... }

    /// Returns the `AveragingWindowTimespan` for `network` and `height`.
    pub fn averaging_window_timespan_for_height(network: Network,
                                                height: block::Height)
                                                -> Duration { ... }
}
```

`averaging_window_timespan` and `averaging_window_timespan_for_height` are
located in the existing `zebra_chain::parameters::network_upgrade` module.

### Test network minimum difficulty calculation
[test-net-min-difficulty-calculation]: #test-net-min-difficulty-calculation

A block is a testnet minimum difficulty block if:
* the block is a testnet block,
* the block's height is 299188 or greater, and
* the time gap from the previous block is greater than the testnet minimum
  difficulty gap, which is 6 times the target spacing for the block's height.
  (The target spacing was halved from the Blossom network upgrade onwards.)

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
threshold value, the modification of the `difficulty_threshold` (`nBits`) field,
and its use in future difficulty adjustments were all incorrect. These errors are
fixed in [ZIP PR 417] and [ZIP commit 806076c].

[ZIP-208]: https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
[ZIP PR 417]: https://github.com/zcash/zips/pull/417
[ZIP commit 806076c]: https://github.com/zcash/zips/commit/806076c48c9834fd9941b940a32310d737975a3a

#### Test network minimum difficulty implementation
[test-net-min-difficulty-implementation]: #test-net-min-difficulty-implementation

The testnet minimum difficulty calculation uses the existing
`NetworkUpgrade::minimum_difficulty_spacing_for_height` function to calculate the
minimum difficulty gap.

In Zcash, the testnet minimum difficulty rule starts at block 299188, and in
Zebra, contextual validation starts after Sapling activation. So we can assume
that there is always a previous block.

```rust
/// Returns true if the gap between the candidate block's `time` and the previous
/// block's `previous_time` is greater than the testnet minimum difficulty time
/// gap for `network` and `height`.
///
/// Returns false for `Mainnet`, when the `height` is below the testnet minimum
/// difficulty start height, and when the time gap is too small.
///
/// `time` can be less than, equal to, or greater than `previous_time`, because
/// block times are provided by miners.
///
/// Implements the testnet minimum difficulty adjustment from ZIPs 205 and 208.
///
/// Spec Note: Some parts of ZIPs 205 and 208 previously specified an incorrect
/// check for the time gap. This function implements the correct "greater than"
/// check.
fn is_testnet_min_difficulty_block(time: DateTime<Utc>,
                                   network: Network,
                                   height: block::Height,
                                   previous_time: DateTime<Utc>)
                                   -> bool { ... }
```

`is_testnet_min_difficulty_block` is located in the existing
`zebra_consensus::work::difficulty` module.

### Block difficulty threshold calculation
[block-difficulty-threshold-calculation]: #block-difficulty-threshold-calculation

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the averaging
window timespan.

The result of the scaled threshold calculation is limited by
`ToCompact(PoWLimit(network))`, a per-network minimum block difficulty. This
minimum difficulty is also used when a testnet block's time gap exceeds the
minimum difficulty gap. We use the existing
`ExpandedDifficulty::target_difficulty_limit` function to calculate the value of
`ToCompact(PoWLimit(network))`.

In Zebra, contextual validation starts after Sapling activation, so the genesis
case of `Threshold()` in the Zcash specification is unreachable.

Note that `zcashd` truncates the `MeanTarget` after the mean calculation, and
after dividing by `AveragingWindowTimespan`. But as long as there is no overflow,
this is [equivalent to the single truncation of the final result] in the Zcash
specification. However, Zebra should follow the order of operations in `zcashd`,
and use repeated divisions, because that can't overflow.

[equivalent to the single truncation of the final result]: https://math.stackexchange.com/questions/147771/rewriting-repeated-integer-division-with-multiplication

#### Avoiding overflow in the block difficulty threshold calculation
[block-difficulty-threshold-overflow]: #block-difficulty-threshold-overflow

Since:
* the `PoWLimit`s are `2^251 − 1` for Testnet, and `2^243 − 1` for mainnet,
* the `ActualTimespanBounded` can be at most `MaxActualTimespan`, which is
  `floor(PoWAveragingWindow * PoWTargetSpacing * (1 + PoWMaxAdjustDown))` or
  `floor(17 * 150 * (1 + 32/100)) =  3366`,
* `AveragingWindowTimespan` is at most `17 * 150 = 2250`, and
* `MeanTarget` is at most `PoWLimit`, ...

The maximum scaled value inside the `Threshold()` calculation is:
* `floor(PoWLimit / 2250) * 3366`, which equals
* `floor((2^251 − 1) / 2250) * 3366`, which equals
* `(2^251 − 1) * 132/100`,
* which is less than `2^252`.

Therefore, this calculation can not overflow a `u256` value. (And even if it did
overflow, it would be constrained to a valid value by the `PoWLimit` minimum.)

Note that the multiplication by `ActualTimespanBounded` must happen after the
division by `AveragingWindowTimespan`. Performing the multiplication first
could overflow.

#### Block difficulty threshold implementation
[block-difficulty-threshold-implementation]: #block-difficulty-threshold-implementation

```rust
/// Calculate the `difficulty_threshold` for a candidate block, based on that
/// block's `time`, `network` and `height`, and some `context` data from recent
/// blocks.
///
/// The `context` contains the `difficulty_threshold`s and `time`s from the
/// previous `PoWAveragingWindow + PoWMedianBlockSpan` blocks in the relevant
/// chain, in reverse height order, starting with the parent block.
///
/// Implements `ThresholdBits` from the Zcash specification, including  the
/// testnet minimum difficulty adjustment from ZIPs 205 and 208.
pub fn difficulty_threshold_from_context(time: DateTime<Utc>,
                                         network: Network,
                                         height: block::Height,
                                         context: &[(CompactDifficulty, DateTime<Utc>); 28])
                                         -> CompactDifficulty { ... }

/// Calculate the `difficulty_threshold` for a candidate block, based on that
/// block's `network` and `height`, and some `context` data from recent blocks.
///
/// See `difficulty_threshold_from_context` for details.
///
/// Implements `ThresholdBits` from the Zcash specification. (Excluding the
/// testnet minimum difficulty adjustment.)
fn difficulty_threshold_bits(network: Network,
                             height: block::Height,
                             context: &[(CompactDifficulty, DateTime<Utc>); 28])
                             -> CompactDifficulty { ... }
```

`difficulty_threshold_from_context` and `difficulty_threshold_bits` are located
in the existing `zebra_consensus::work::difficulty` module.

## Remaining TODOs for Reference-level explanation

This is the technical portion of the RFC. Explain the design in sufficient detail that:

- [x] Its interaction with other features is clear.
- It is reasonably clear how the feature would be:
  - [x] implemented,
  - [ ] tested,
  - [ ] monitored, and
  - [ ] maintained.
- [ ] Corner cases are dissected by example.

- [ ] The section should return to the examples given in the previous section, and explain more fully how the detailed proposal makes those examples work.

## Module Structure

- [ ] ~Describe~ Summarise the crate and modules that will implement the feature.

## Test Plan

Explain how the feature will be tested, including:
- [ ] tests for consensus-critical functionality
- [ ] existing test vectors, if available
- [ ] Zcash blockchain block test vectors (specify the network upgrade, feature, or block height and network)
- [ ] property testing or fuzzing

The tests should cover:
- [ ] positive cases: make sure the feature accepts valid inputs
  - using block test vectors for each network upgrade provides some coverage of valid inputs
- [ ] negative cases: make sure the feature rejects invalid inputs
  - make sure there is a test case for each error condition in the code
  - if there are lots of potential errors, prioritise:
    - consensus-critical errors
    - security-critical errors, and
    - likely errors
- [ ] edge cases: make sure that boundary conditions are correctly handled

# Drawbacks
[drawbacks]: #drawbacks

- [ ] Why should we *not* do this?

## Alternate consensus parameters

Any alternate consensus parameters or `regtest` mode would have to respect the constraints set by this design.

In particular:
  * the `PoWLimit` must be less than `(2^256 - 1) / PoWAveragingWindow` to avoid overflow,
  * the `PoWAveragingWindow` and `PoWMedianBlockSpan` are fixed by function argument types
    (at least until Rust gets generic slice lengths), and
  * the design eliminates a significant number of edge cases by assuming that difficulty adjustments aren't
    validated for the first `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the chain.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

- [ ] What makes this design a good design?
- [ ] Is this design a good basis for later designs or implementations?
- [ ] What other designs have been considered and what is the rationale for not choosing them?
- [ ] What is the impact of not doing this?

Zebra could accept invalid, low-difficulty blocks from arbitrary miners. That would be a security issue.

# Prior art
[prior-art]: #prior-art

Discuss prior art, both the good and the bad, in relation to this proposal.
A few examples of what this can include are:

- [ ] zcashd
- [ ] Zcash specification
- [ ] Bitcoin?

Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC.
Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features and designs.

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- [ ] What parts of the design do you expect to resolve through the RFC process before this gets merged?
- [ ] What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
- [ ] What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?

# Future possibilities
[future-possibilities]: #future-possibilities

- [ ] Relevant chain iterator as a general basis for contextual validation
- [ ] `difficulty_threshold_is_valid` as a basis for header-only validation
- [ ] Talk about other future possibilities (if there are any)

## Caching difficulty calculations

Difficulty calculations use `u256` could be a bit expensive, particularly if we
get a flood of low-difficulty blocks. To reduce the impact of this kind of DoS, we could
cache the value returned by `difficulty_threshold_bits` for each block in the
non-finalized state, and the finalized tip.

## Future possibilities template text

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
