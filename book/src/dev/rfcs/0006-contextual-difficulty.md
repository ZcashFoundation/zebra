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
  block's difficulty field).

* **block work**: The approximate amount of work required for a miner to generate
  a block hash that passes the difficulty filter. The number of block header
  attempts and the mining time are proportional to the work value. Numerically
  higher work values represent longer processing times.

* **averaging window**: The 17 most recent blocks in the relevant chain.

* **median block span**: The 11 most recent blocks from a chosen tip, typically
  the relevant tip.

* **target spacing**: 150 seconds per block before Blossom activation, 75 seconds
  per block from Blossom activation onwards.

* **adjusted difficulty**: After each block is mined, the difficulty threshold of
  the next block is adjusted, to keep the block gap close to the target spacing.

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

Zcash's difficulty consensus rules are similar to Bitcoin.

Each block contains a **difficulty threshold** in its header. The hash of the
block header must be less than this **difficulty threshold**. (When interpreted
as a 256-bit integer in big-endian byte order.) This context-free semantic
verification check is performed by the `BlockVerifier`.

After each block, the difficulty threshold is adjusted so that the block gap is
close to the target spacing. On average, harder blocks take longer to mine, and
easier blocks take less time.

The **adjusted difficulty** for the next block is calculated using the difficulty
thresholds and times of recent blocks. Zcash uses the most recent 28 blocks in
the **relevant chain** in its difficulty adjustment calculations.

The difficulty adjustment calculations adjust the **mean target difficulty**,
based on the difference between the **median timespan** and the
**target timespan**. If the median timespan is less than the target timespan, the
next block is harder to mine.

The `StateService` calculates the adjusted difficulty using the context from the
**relevant chain**. The difficulty contextual verification check ensures that the
**difficulty threshold** of the next block is equal to the **adjusted difficulty**
for its relevant chain.

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
finalized tip. See [RFC5] for details.

[RFC5]: ./0005-state-updates.md

## Contextual validation design
[contextual-validation-design]: #contextual-validation-design

Contextual validation is performed synchronously by the state service, as soon
as the state has:
* received the semantically valid next block (via `CommitBlock`), and
* committed the previous block.

The difficulty adjustment check calculates the correct adjusted difficulty
threshold value for a candidate block, and ensures that the block's
`difficulty_threshold` field is equal to that value.

This check is implemented as follows:

### Difficulty adjustment
[difficulty-adjustment]: #difficulty-adjustment

The block difficulty threshold is adjusted by scaling the mean target difficulty
by the median timespan.

On Testnet, if a long time has elapsed since the previous block, the difficulty
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

If there is a large gap after a Testnet block, the next block becomes a minimum
difficulty block. Testnet minimum difficulty blocks have their
`difficulty_threshold` set to the minimum difficulty for Testnet.

#### Block difficulty threshold
[block-difficulty-threshold]: #block-difficulty-threshold

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the averaging
window timespan.

The result of this calculation is limited by `ToCompact(PoWLimit(network))`, a
per-network minimum block difficulty. This minimum difficulty is also used when
a Testnet block's time gap exceeds the minimum difficulty gap.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Contextual validation
[contextual-validation]: #contextual-validation

Contextual validation is implemented in
`StateService::check_contextual_validity`, which calls a separate function for
each contextual validity check.

In Zebra, contextual validation starts after Canopy activation, so we can assume
that the relevant chain contains at least 28 blocks on Mainnet and Testnet. (And
panic if this assumption does not hold at runtime.)

## Fundamental data types
[fundamental-data-types]: #fundamental-data-types

Zebra is free to implement its difficulty calculations in any way that produces
equivalent results to `zcashd` and the Zcash specification.

### Difficulty

In Zcash block headers, difficulty thresholds are stored as a "compact" `nBits`
value, which uses a custom 32-bit floating-point encoding. Zebra calls this type
`CompactDifficulty`.

In Zcash, difficulty threshold calculations are performed using unsigned 256-bit
integers. Rust has no standard `u256` type, but there are a number of crates
available which implement the required operations on 256-bit integers. Zebra
abstracts over the chosen `u256` implementation using its `ExpandedDifficulty`
type.

### Time

In Zcash, time values are unsigned 32-bit integers. But the difficulty adjustment
calculations include time subtractions which could overflow an unsigned type, so
they are performed using signed 64-bit integers in `zcashd`.

Zebra parses the `header.time` field into a `DateTime<Utc>`. Conveniently, the
`chrono::DateTime<_>::timestamp()` function returns `i64` values. So Zebra can do
its signed time calculations using `i64` values internally.

Note: `i32` is an unsuitable type for signed time calculations. It is
theoretically possible for the time gap between blocks to be larger than
`i32::MAX`, because those times are provided by miners. Even if the median time
gap is that large, the bounds and minimum difficulty in Zcash's difficulty
adjustment algorithm will preserve a reasonable difficulty threshold. So Zebra
must support this edge case.

### Consensus-Critical Operations

The order of operations and overflow semantics for 256-bit integers can be
consensus-critical.

For example:
  - dividing before multiplying discards lower-order bits, but
  - multiplying before dividing can cause overflow.

Zebra's implementation should try to match zcashd's order of operations and
overflow handling as closely as possible.

## Difficulty adjustment check
[difficulty-adjustment-check]: #difficulty-adjustment-check

The difficulty adjustment check calculates the correct difficulty threshold
value for a candidate block, and ensures that the block's
`difficulty_threshold` field is equal to that value.

### Context data type
[context-data-type]: #context-data-type

The difficulty adjustment functions use a context consisting of the difficulties
and times from the previous 28 blocks in the relevant chain.

These functions also use the candidate block's `height` and `network`.

To make these functions more ergonomic, we create a `AdjustedDifficulty`
type, and implement the difficulty adjustment calculations as methods on that
type.

```rust
/// The averaging window for difficulty threshold arithmetic mean calculations.                               
///                                                                                                           
/// `PoWAveragingWindow` in the Zcash specification.                                                          
pub const POW_AVERAGING_WINDOW: usize = 17;

/// The median block span for time median calculations.                                                       
///                                                                                                           
/// `PoWMedianBlockSpan` in the Zcash specification.                                                          
pub const POW_MEDIAN_BLOCK_SPAN: usize = 11;

/// Contains the context needed to calculate the adjusted difficulty for a block. 
struct AdjustedDifficulty {
    candidate_time: DateTime<Utc>,
    candidate_height: block::Height,
    network: Network,
    relevant_difficulty_thresholds: [CompactDifficulty; POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN],
    relevant_times: [DateTime<Utc>; POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN],
}
```

We implement some initialiser methods on `AdjustedDifficulty` for convenience.
We might want to validate downloaded headers in future, so we include a
`new_from_header` initialiser.

```rust
/// Initialise and return a new `AdjustedDifficulty` using a `candidate_block`,
/// `network`, and a `context`.
///
/// The `context` contains the previous
/// `PoWAveragingWindow + PoWMedianBlockSpan` (28) `difficulty_threshold`s and
/// `time`s from the relevant chain for `candidate_block`, in reverse height
/// order, starting with the previous block.
///
/// Note that the `time`s might not be in reverse chronological order, because
/// block times are supplied by miners.
///
/// Panics:
/// If the `context` contains fewer than 28 items.
pub fn new_from_block<C>(candidate_block: &Block,
                         network: Network,
                         context: C)
                         -> AdjustedDifficulty
    where
        C: IntoIterator<Item = (CompactDifficulty, DateTime<Utc>)>,
    { ... }

/// Initialise and return a new `AdjustedDifficulty` using a
/// `candidate_header`, `previous_block_height`, `network`, and a `context`.
///
/// Designed for use when validating block headers, where the full block has not
/// been downloaded yet.
///
/// See `new_from_block` for detailed information about the `context`.
///
/// Panics:
/// If the context contains fewer than 28 items.
pub fn new_from_header<C>(candidate_header: &block::Header,
                          previous_block_height: block::Height,
                          network: Network,
                          context: C)
                          -> AdjustedDifficulty
    where
        C: IntoIterator<Item = (CompactDifficulty, DateTime<Utc>)>,
    { ... }
```

#### Memory usage note

Copying `CompactDifficulty` values into the `AdjustedDifficulty` struct uses
less memory than borrowing those values. `CompactDifficulty` values are 32 bits,
but pointers are 64-bit on most modern machines. (And since they all come from
different blocks, we need a pointer to each individual value.)

Borrowing `DateTime<Utc>` values might use slightly less memory than copying
them - but that depends on the exact way that Rust stores associated types
derived from a generic argument.

In any case, the overall size of each `AdjustedDifficulty` is only a few
hundred bytes. If it turns up in profiles, we can look at borrowing the block
header data.

### Difficulty adjustment check implementation
[difficulty-adjustment-check-implementation]: #difficulty-adjustment-check-implementation

The difficulty adjustment check ensures that the
`candidate_difficulty_threshold` is equal to the `difficulty_threshold` value
calculated using `AdjustedDifficulty::adjusted_difficulty_threshold`.

We implement this function:
```rust
/// Validate the `difficulty_threshold` from a candidate block's header, based
/// on an `expected_difficulty` for that block.
///
/// Uses `expected_difficulty` to calculate the expected `ToCompact(Threshold())`
/// value, then compares that value to the `difficulty_threshold`. Returns
/// `Ok(())` if the values are equal.
pub fn difficulty_threshold_is_valid(difficulty_threshold: CompactDifficulty,
                                     expected_difficulty: AdjustedDifficulty)
                                     -> Result<(), ValidateContextError> { ... }
```

[Issue 1166]: https://github.com/ZcashFoundation/zebra/issues/1166

### Mean target difficulty calculation
[mean-target-difficulty-calculation]: #mean-target-difficulty-calculation

The mean target difficulty is the arithmetic mean of the difficulty
thresholds of the `PoWAveragingWindow` (17) most recent blocks in the relevant
chain.

We implement this method on `AdjustedDifficulty`:
```rust
/// Calculate the arithmetic mean of the averaging window thresholds: the
/// expanded `difficulty_threshold`s from the previous `PoWAveragingWindow` (17)
/// blocks in the relevant chain.
///
/// Implements `MeanTarget` from the Zcash specification.
fn mean_target_difficulty(&self) -> ExpandedDifficulty { ... }
```

#### Implementation notes

Since the `PoWLimit`s are `2^251 − 1` for Testnet, and `2^243 − 1` for Mainnet,
the sum of these difficulty thresholds will be less than or equal to
`(2^251 − 1)*17 = 2^255 + 2^251 - 17`. Therefore, this calculation can not
overflow a `u256` value. So the function is infallible.

In Zebra, contextual validation starts after Canopy activation, so we can assume
that the relevant chain contains at least 17 blocks. Therefore, the `PoWLimit`
case of `MeanTarget()` in the Zcash specification is unreachable.

### Median timespan calculation
[median-timespan-calculation]: #median-timespan-calculation

The median timespan is the difference of the median times for:
* the relevant tip: the `PoWMedianBlockSpan` (11) most recent blocks, and
* the 11 blocks after the 17-block `PoWAveragingWindow`: that is, blocks 18-28
  behind the relevant tip.

(The median timespan is known as the `ActualTimespan` in the Zcash specification,
but this terminology is confusing, because it is a difference of medians, rather
than any "actual" elapsed time.)

Zebra implements the median timespan using the following methods on
`AdjustedDifficulty`:
```rust
/// Calculate the bounded median timespan. The median timespan is the
/// difference of medians of the timespan times, which are the `time`s from
/// the previous `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the
/// relevant chain.
///
/// Uses the candidate block's `height' and `network` to calculate the
/// `AveragingWindowTimespan` for that block.
///
/// The median timespan is damped by the `PoWDampingFactor`, and bounded by
/// `PoWMaxAdjustDown` and `PoWMaxAdjustUp`.
///
/// Implements `ActualTimespanBounded` from the Zcash specification.
///
/// Note: This calculation only uses `PoWMedianBlockSpan` (11) times at the
/// start and end of the timespan times. timespan times `[11..=16]` are ignored.
fn median_timespan_bounded(&self) -> Duration { ... }

/// Calculate the median timespan. The median timespan is the difference of
/// medians of the timespan times, which are the `time`s from the previous
/// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
///
/// Implements `ActualTimespan` from the Zcash specification.
///
/// See `median_timespan_bounded` for details.
fn median_timespan(&self) -> Duration { ... }

/// Calculate the median of the `median_block_span_times`: the `time`s from a
/// slice of `PoWMedianBlockSpan` (11) blocks in the relevant chain.
///
/// Implements `MedianTime` from the Zcash specification.
fn median_time(mut median_block_span_times: [DateTime<Utc>; POW_MEDIAN_BLOCK_SPAN])
               -> DateTime<Utc> { ... }
```

Zebra implements the `AveragingWindowTimespan` using the following methods on
`NetworkUpgrade`:
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

#### Implementation notes

In Zebra, contextual validation starts after Canopy activation, so we can assume
that the relevant chain contains at least 28 blocks. Therefore:
* `max(0, height − PoWMedianBlockSpan)` in the `MedianTime()` calculation
   simplifies to `height − PoWMedianBlockSpan`, and
* there is always an odd number of blocks in `MedianTime()`, so the median is
  always the exact middle of the sequence.

Therefore, the function is infallible.

### Test network minimum difficulty calculation
[test-net-min-difficulty-calculation]: #test-net-min-difficulty-calculation

A block is a Testnet minimum difficulty block if:
* the block is a Testnet block,
* the block's height is 299188 or greater, and
* the time gap from the previous block is greater than the Testnet minimum
  difficulty gap, which is 6 times the target spacing for the block's height.
  (The target spacing was halved from the Blossom network upgrade onwards.)

The difficulty adjustment is modified for Testnet minimum difficulty blocks as
follows:
* the difficulty threshold in the block header is set to the Testnet minimum
  difficulty threshold, `ToCompact(PoWLimit(network))`.

Since the new difficulty changes the block header, Testnet blocks can only
satisfy one of the alternate difficulty adjustment rules:
* if the time gap is less than or equal to the Testnet minimum difficulty gap:
  the difficulty threshold is calculated using the default difficulty adjustment
  rule,
* if the time gap is greater than the Testnet minimum difficulty gap:
  the difficulty threshold is the Testnet minimum difficulty threshold.

See [ZIP-208] for details.

Note: some older versions of ZIPs 205 and 208 incorrectly said that:
* the time gap threshold uses an "at least" check (it is strictly greater than),
* the minimum difficulty threshold value was `PoWLimit`
  (it is `ToCompact(PoWLimit)`),
* the `difficulty_threshold` (`nBits`) field is not modified in Testnet minimum
  difficulty blocks (the field is modified), and
* the Testnet minimum difficulty value is not used to calculate future difficulty
  adjustments (the modified value is used in future adjustments).

ZIP 205 and 208 were fixed on 14 November 2020, see [ZIP PR 417] and
[ZIP commit 806076c] for details.

[ZIP-208]: https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
[ZIP PR 417]: https://github.com/zcash/zips/pull/417
[ZIP commit 806076c]: https://github.com/zcash/zips/commit/806076c48c9834fd9941b940a32310d737975a3a

#### Test network minimum difficulty implementation
[test-net-min-difficulty-implementation]: #test-net-min-difficulty-implementation

The Testnet minimum difficulty calculation uses the existing
`NetworkUpgrade::minimum_difficulty_spacing_for_height` function to calculate the
minimum difficulty gap.

We implement this method on `NetworkUpgrade`:
```rust
/// Returns true if the gap between `block_time` and `previous_block_time` is                             
/// greater than the Testnet minimum difficulty time gap. This time gap                                   
/// depends on the `network` and `block_height`.                                                          
///                                                                                                       
/// Returns false on Mainnet, when `block_height` is less than the minimum                                
/// difficulty start height, and when the time gap is too small.                                          
///                                                                                                       
/// `block_time` can be less than, equal to, or greater than                                              
/// `previous_block_time`, because block times are provided by miners.                                    
///                                                                                                       
/// Implements the Testnet minimum difficulty adjustment from ZIPs 205 and 208.                           
///                                                                                                       
/// Spec Note: Some parts of ZIPs 205 and 208 previously specified an incorrect                           
/// check for the time gap. This function implements the correct "greater than"                           
/// check.                                                                                                
pub fn is_testnet_min_difficulty_block(
    network: Network,
    block_height: block::Height,
    block_time: DateTime<Utc>,
    previous_block_time: DateTime<Utc>,
) -> bool { ... }
```

#### Implementation notes

In Zcash, the Testnet minimum difficulty rule starts at block 299188, and in
Zebra, contextual validation starts after Canopy activation. So we can assume
that there is always a previous block.

Therefore, this function is infallible.

### Block difficulty threshold calculation
[block-difficulty-threshold-calculation]: #block-difficulty-threshold-calculation

The block difficulty threshold for the next block is calculated by scaling the
mean target difficulty by the ratio between the median timespan and the averaging
window timespan.

The result of the scaled threshold calculation is limited by
`ToCompact(PoWLimit(network))`, a per-network minimum block difficulty. This
minimum difficulty is also used when a Testnet block's time gap exceeds the
minimum difficulty gap. We use the existing
`ExpandedDifficulty::target_difficulty_limit` function to calculate the value of
`ToCompact(PoWLimit(network))`.

In Zebra, contextual validation starts after Canopy activation, so the genesis
case of `Threshold()` in the Zcash specification is unreachable.

#### Block difficulty threshold implementation
[block-difficulty-threshold-implementation]: #block-difficulty-threshold-implementation

We implement these methods on `AdjustedDifficulty`:
```rust
/// Calculate the expected `difficulty_threshold` for a candidate block, based
/// on the `candidate_time`, `candidate_height`, `network`, and the
/// `difficulty_threshold`s and `time`s from the previous
/// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
///
/// Implements `ThresholdBits` from the Zcash specification, and the Testnet
/// minimum difficulty adjustment from ZIPs 205 and 208.
pub fn expected_difficulty_threshold(&self) -> CompactDifficulty { ... }

/// Calculate the `difficulty_threshold` for a candidate block, based on the
/// `candidate_height`, `network`, and the relevant `difficulty_threshold`s and
/// `time`s.
///
/// See `expected_difficulty_threshold` for details.
///
/// Implements `ThresholdBits` from the Zcash specification. (Which excludes the
/// Testnet minimum difficulty adjustment.)
fn threshold_bits(&self) -> CompactDifficulty { ... }
```

#### Implementation notes

Since:
* the `PoWLimit`s are `2^251 − 1` for Testnet, and `2^243 − 1` for Mainnet,
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

If implemented in this way, the function is infallible.

`zcashd` truncates the `MeanTarget` after the mean calculation, and
after dividing by `AveragingWindowTimespan`. But as long as there is no overflow,
this is [equivalent to the single truncation of the final result] in the Zcash
specification. However, Zebra should follow the order of operations in `zcashd`,
and use repeated divisions, because that can't overflow. See the relevant
[comment in the zcashd source code].

[equivalent to the single truncation of the final result]: https://math.stackexchange.com/questions/147771/rewriting-repeated-integer-division-with-multiplication
[comment in the zcashd source code]: https://github.com/zcash/zcash/pull/4860/files

## Module Structure
[module-structure]: #module-structure

The structs and functions in this RFC are implemented in a new
`zebra_state::service::check::difficulty` module.

This module has two entry points:
* `DifficultyAdjustment::new_from_block`
* `difficulty_threshold_is_valid`

These entry points are both called from
`StateService::check_contextual_validity`.

## Test Plan
[test-plan]: #test-plan

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

Why should we *not* do this?

## Alternate consensus parameters
[alternate-consensus-parameters]: #alternate-consensus-parameters

Any alternate consensus parameters or `regtest` mode would have to respect the constraints set by this design.

In particular:
  * the `PoWLimit` must be less than or equal to
    `(2^256 - 1) / PoWAveragingWindow` (approximately `2^251`) to avoid overflow,
  * the `PoWAveragingWindow` and `PoWMedianBlockSpan` are fixed by function argument types
    (at least until Rust gets stable const generics), and
  * the design eliminates a significant number of edge cases by assuming that difficulty adjustments aren't
    validated for the first `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the chain.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

## Is this design a good basis for later designs or implementations?
[good-basis]: #good-basis

The design includes specific methods for a future header-only validation design.

## What other designs have been considered and what is the rationale for not choosing them?
[alternate-designs]: #alternate-designs

A previous version of the RFC did not have the `AdjustedDifficulty` struct and
methods. That design was easy to misuse, because each function had a complicated
argument list.

## What is the impact of not doing this?
[no-action]: #no-action

Zebra could accept invalid, low-difficulty blocks from arbitrary miners. That
would be a security issue.

# Prior art
[prior-art]: #prior-art

* `zcashd`
* the Zcash specification
* Bitcoin

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- What parts of the design do you expect to resolve through the implementation of this feature before stabilization?
  - Guide-level examples
  - Reference-level examples
  - Corner case examples
  - Testing

- What related issues do you consider out of scope for this RFC that could be addressed in the future independently of the solution that comes out of this RFC?
  - Monitoring and maintenance

# Future possibilities
[future-possibilities]: #future-possibilities

## Reusing the relevant chain API in other contextual checks
[relevant-chain-api-reuse]: #relevant-chain-api-reuse

The relevant chain iterator can be reused to implement other contextual
validation checks.

For example, responding to peer requests for block locators, which means
implementing relevant chain hash queries as a `StateService` request

## Header-only difficulty adjustment validation
[header-only-validation]: #header-only-validation

Implementing header-only difficulty adjustment validation as a `StateService` request.

## Caching difficulty calculations
[caching-calculations]: #caching-calculations

Difficulty calculations use `u256` could be a bit expensive, particularly if we
get a flood of low-difficulty blocks. To reduce the impact of this kind of DoS,
we could cache the value returned by `threshold_bits` for each block in the
non-finalized state, and for the finalized tip. This value could be used to
quickly calculate the difficulties for any child blocks of these blocks.

There's no need to persist this cache, or pre-fill it. (Minimum-difficulty
Testnet blocks don't call `threshold_bits`, and some side-chain blocks will
never have a next block.)

This caching is only worth implementing if these calculations show up in `zebrad`
profiles.
