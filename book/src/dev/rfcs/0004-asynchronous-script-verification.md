- Start Date: 2020-08-10
- Design PR: [ZcashFoundation/zebra#868](https://github.com/ZcashFoundation/zebra/pull/868)
- Zebra Issue: [ZcashFoundation/zebra#964](https://github.com/ZcashFoundation/zebra/issues/964)

# Summary

[summary]: #summary

This RFC describes an architecture for asynchronous script verification and
its interaction with the state layer. This architecture imposes constraints
on the ordering of operations in the state layer.

# Motivation

[motivation]: #motivation

As in the rest of Zebra, we want to express our work as a collection of
work-items with explicit dependencies, then execute these items concurrently
and in parallel on a thread pool.

# Definitions

[definitions]: #definitions

- _UTXO_: unspent transparent transaction output.
  Transparent transaction outputs are modeled in `zebra-chain` by the [`transparent::Output`][transout] structure.
- outpoint: a reference to an unspent transparent transaction output, including a transaction hash and output index.
  Outpoints are modeled in `zebra-chain` by the [`transparent::OutPoint`][outpoint] structure.
- transparent input: a previous transparent output consumed by a later transaction (the one it is an input to).
  Modeled in `zebra-chain` by the [`transparent::Input::PrevOut`][transin] enum variant.
- coinbase transaction: the first transaction in each block, which creates new coins.
- lock script: the script that defines the conditions under which some UTXO can be spent.
  Stored in the [`transparent::Output::lock_script`][lock_script] field.
- unlock script: a script satisfying the conditions of the lock script, allowing a UTXO to be spent.
  Stored in the [`transparent::Input::PrevOut::lock_script`][lock_script] field.

[transout]: https://docs.rs/zebra_chain/latest/zebra_chain/transparent/struct.Output.html
[outpoint]: https://docs.rs/zebra_chain/latest/zebra_chain/transparent/struct.OutPoint.html
[lock_script]: https://docs.rs/zebra_chain/latest/zebra_chain/transparent/struct.Output.html#structfield.lock_script
[transin]: https://docs.rs/zebra_chain/latest/zebra_chain/transparent/enum.Input.html
[unlock_script]: https://docs.rs/zebra_chain/latest/zebra_chain/transparent/enum.Input.html#variant.PrevOut.field.unlock_script

# Guide-level explanation

[guide-level-explanation]: #guide-level-explanation

Zcash's transparent address system is inherited from Bitcoin. Transactions
spend unspent transparent transaction outputs (UTXOs) from previous transactions. These
UTXOs are encumbered by _locking scripts_ that define the conditions under
which they can be spent, e.g., requiring a signature from a certain key.
Transactions wishing to spend UTXOs supply an _unlocking script_ that should
satisfy the conditions of the locking script for each input they wish to
spend.

This means that script verification requires access to data about previous
UTXOs, in order to determine the conditions under which those UTXOs can be
spent. In Zebra, we aim to run operations asynchronously and out-of-order to
the greatest extent possible. For instance, we may begin verification of a
block before all of its ancestors have been verified or even downloaded. So
we need to design a mechanism that allows script verification to declare its
data dependencies and execute as soon as all required data is available.

It's not necessary for this mechanism to ensure that the transaction outputs
remain unspent, only to give enough information to perform script
verification. Checking that all transaction inputs are actually unspent is
done later, at the point that its containing block is committed to the chain.

At a high level, this adds a new request/response pair to the state service:

- `Request::AwaitSpendableUtxo { output: OutPoint, ..conditions }`
  requests a spendable `transparent::Output`, looked up using `OutPoint`.
- `Response::SpendableUtxo(Utxo)` supplies the requested `transparent::Output`
  as part of a new `Utxo` type,
  if the output is spendable based on `conditions`;

Note that this request is named differently from the other requests,
`AwaitSpendableUtxo` rather than `GetUtxo` or similar. This is because the
request has rather different behavior:

- the request does not complete until the state service learns about a UTXO
  matching the request, which could be never. For instance, if the transaction
  output was already spent, the service is not required to return a response.
- the request does not complete until the output is spendable, based on the
  `conditions` in the request.

The state service does not cancel long-running UTXO requests. Instead, the caller
is responsible for deciding when a request is unlikely to complete. (For example,
using a timeout layer.)

This allows a script verifier to asynchronously obtain information about
previous transaction outputs and start verifying scripts as soon as the data
is available. For instance, if we begin parallel download and verification of
500 blocks, we should be able to begin script verification of all scripts
referencing outputs from existing blocks in parallel, and begin verification
of scripts referencing outputs from new blocks as soon as they are committed
to the chain.

Because spending outputs from older blocks is more common than spending
outputs from recent blocks, this should allow a significant amount of
parallelism.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

## Data structures

[data-structures]: #data-structures

We add the following request and response to the state protocol:

```rust
enum Request::AwaitSpendableUtxo {
    outpoint: OutPoint,
    spend_height: Height,
    spend_restriction: SpendRestriction,
}

/// Consensus rule:
/// "A transaction with one or more transparent inputs from coinbase transactions
/// MUST have no transparent outputs (i.e.tx_out_count MUST be 0)."
enum SpendRestriction {
    /// The UTXO is spent in a transaction with transparent outputs
    SomeTransparentOutputs,
    /// The UTXO is spent in a transaction with all shielded outputs
    AllShieldedOutputs,
}
```

As described above, the request name is intended to indicate the request's behavior.
The request does not resolve until:

- the state layer learns of a UTXO described by the request, and
- the output is spendable at `height` with `spend_restriction`.

The new `Utxo` type adds a coinbase flag and height to `transparent::Output`s
that we look up in the state, or get from newly committed blocks:

```rust
enum Response::SpendableUtxo(Utxo)

pub struct Utxo {
    /// The output itself.
    pub output: transparent::Output,

    /// The height at which the output was created.
    pub height: block::Height,

    /// Whether the output originated in a coinbase transaction.
    pub from_coinbase: bool,
}
```

## Transparent coinbase consensus rules

[transparent-coinbase-consensus-rules]: #transparent-coinbase-consensus-rules

Specifically, if the UTXO is a transparent coinbase output,
the service is not required to return a response if:

- `spend_height` is less than `MIN_TRANSPARENT_COINBASE_MATURITY` (100) blocks after the `Utxo.height`, or
- `spend_restriction` is `SomeTransparentOutputs`.

This implements the following consensus rules:

> A transaction MUST NOT spend a transparent output of a coinbase transaction
> from a block less than 100 blocks prior to the spend.
>
> Note that transparent outputs of coinbase transactions include Founders’ Reward
> outputs and transparent funding stream outputs.

> A transaction with one or more transparent inputs from coinbase transactions
> MUST have no transparent outputs (i.e.tx_out_count MUST be 0).
>
> Inputs from coinbase transactions include Founders’ Reward outputs and funding
> stream outputs.

<https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus>

## Parallel coinbase checks

[parallel-coinbase-checks]: #parallel-coinbase-checks

We can perform these coinbase checks asynchronously, in the presence of multiple chain forks,
as long as the following conditions both hold:

1. We don't mistakenly accept or reject spends to the transparent pool.

2. We don't mistakenly accept or reject mature spends.

### Parallel coinbase justification

[parallel-coinbase-justification]: #parallel-coinbase-justification

There are two parts to a spend restriction:

- the `from_coinbase` flag, and
- if the `from_coinbase` flag is true, the coinbase `height`.

If a particular transaction hash `h` always has the same `from_coinbase` value,
and `h` exists in multiple chains, then regardless of which `Utxo` arrives first,
the outputs of `h` always get the same `from_coinbase` value during validation.
So spends can not be mistakenly accepted or rejected due to a different coinbase flag.

Similarly, if a particular coinbase transaction hash `h` always has the same `height` value,
and `h` exists in multiple chains, then regardless of which `Utxo` arrives first,
the outputs of `h` always get the same `height` value during validation.
So coinbase spends can not be mistakenly accepted or rejected due to a different `height` value.
(The heights of non-coinbase outputs are irrelevant, because they are never checked.)

These conditions hold as long as the following multi-chain properties are satisfied:

- `from_coinbase`: across all chains, the set of coinbase transaction hashes is disjoint from
  the set of non-coinbase transaction hashes, and
- coinbase `height`: across all chains, duplicate coinbase transaction hashes can only occur at
  exactly the same height.

### Parallel coinbase consensus rules

[parallel-coinbase-consensus]: #parallel-coinbase-consensus

These multi-chain properties can be derived from the following consensus rules:

Transaction versions 1-4:

> [Pre-Sapling ] If effectiveVersion = 1 or nJoinSplit = 0, then both tx_in_count and tx_out_count MUST be nonzero.
> ...
> [Sapling onward] If effectiveVersion < 5, then at least one of tx_in_count, nSpendsSapling, and nJoinSplit MUST be nonzero.

> A coinbase transaction for a block at block height greater than 0
> MUST have a script that, as its first item, encodes the _block height_ height as follows.
>
> For height in the range {1 .. 16}, the encoding is a single byte of value 0x50 + height.
>
> Otherwise, let heightBytes be the signed little-endian representation of height,
> using the minimum nonzero number of bytes such that the most significant byte is < 0x80.
> The length of heightBytes MUST be in the range {1 .. 8}.
> Then the encoding is the length of heightBytes encoded as one byte, followed by heightBytes itself.

<https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus>

> The transaction ID of a version 4 or earlier transaction is the SHA-256d hash of the transaction encoding in the
> pre-v5 format described above.

<https://zips.z.cash/protocol/protocol.pdf#txnidentifiers>

Transaction version 5:

> [NU5 onward] If effectiveVersion ≥ 5, then this condition must hold: tx_in_count > 0 or nSpendsSapling > 0 or (nActionsOrchard > 0 and enableSpendsOrchard = 1).
> ...
> [NU5 onward] The nExpiryHeight field of a coinbase transaction MUST be equal to its block height.

<https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus>

> non-malleable transaction identifiers ... commit to all transaction data except for attestations to transaction validity
> ...
> A new transaction digest algorithm is defined that constructs the identifier for a transaction from a tree of hashes
> ...
> A BLAKE2b-256 hash of the following values:
> ...
> T.1e: expiry_height (4-byte little-endian block height)

<https://zips.z.cash/zip-0244#t-1-header-digest>

Since:

- coinbase transaction hashes commit to the block `Height`,
- non-coinbase transaction hashes commit to their inputs, and
- double-spends are not allowed;

Therefore:

- coinbase transaction hashes are unique for distinct heights in any chain,
- coinbase transaction hashes are unique in a single chain, and
- non-coinbase transaction hashes are unique in a single chain,
  because they recursively commit to unique inputs.

So the required parallel verification conditions are satisfied.

## Script verification

[script-verification]: #script-verification

To verify scripts, a script verifier requests the relevant UTXOs from the
state service and waits for all of them to resolve, or fails verification
with a timeout error. Currently, we outsource script verification to
`zcash_consensus`, which does FFI into the same C++ code as `zcashd` uses.
**We need to ensure this code is thread-safe**.

## Database implementation

[database-implementation]: #database-implementation

Implementing the state request correctly requires considering two sets of behaviors:

1. behaviors related to the state's external API (a `Buffer`ed `tower::Service`);
2. behaviors related to the state's internal implementation (using `rocksdb`).

Making this distinction helps us to ensure we don't accidentally leak
"internal" behaviors into "external" behaviors, which would violate
encapsulation and make it more difficult to replace `rocksdb`.

In the first category, our state is presented to the rest of the application
as a `Buffer`ed `tower::Service`. The `Buffer` wrapper allows shared access
to a service using an actor model, moving the service to be shared into a
worker task and passing messages to it over an multi-producer single-consumer
(mpsc) channel. The worker task receives messages and makes `Service::call`s.
The `Service::call` method returns a `Future`, and the service is allowed to
decide how much work it wants to do synchronously (in `call`) and how much
work it wants to do asynchronously (in the `Future` it returns).

This means that our external API ensures that the state service sees a
linearized sequence of state requests, although the exact ordering is
unpredictable when there are multiple senders making requests.

Because the state service has exclusive access to the rocksdb database, and the
state service sees a linearized sequence of state requests, we have an easy
way to opt in to asynchronous database access. We can perform rocksdb operations
synchronously in the `Service::call`, waiting for them to complete, and be
sure that all future requests will see the resulting rocksdb state. Or, we can
perform rocksdb operations asynchronously in the future returned by
`Service::call`.

If we perform all _writes_ synchronously and allow reads to be either
synchronous or asynchronous, we ensure that writes cannot race each other.
Asynchronous reads are guaranteed to read at least the state present at the
time the request was processed, or a later state.

## Lookup states

[lookup-states]: #lookup-states

Now, returning to the UTXO lookup problem, we can map out the possible states
with this restriction in mind. This description assumes that UTXO storage is
split into disjoint sets, one in-memory (e.g., blocks after the reorg limit)
and the other in rocksdb (e.g., blocks after the reorg limit). The details of
this storage are not important for this design, only that the two sets are
disjoint.

When the state service processes a `Request::AwaitSpendableUtxo` referencing
some UTXO `u`, there are three disjoint possibilities:

1. `u` is already contained in an in-memory block storage;
2. `u` is already contained in the rocksdb UTXO set;
3. `u` is not yet known to the state service.

In case 3, we need to queue `u` and scan all _future_ blocks to see whether
they contain `u`. However, if we have a mechanism to queue `u`, we can
perform check 2 asynchronously, because restricting to synchronous writes
means that any async read will return the current or later state. If `u` was
in the rocksdb UTXO set when the request was processed, the only way that an
async read would not return `u` is if the UTXO were spent, in which case the
service is not required to return a response.

## Lookup implementation

[lookup-implementation]: #lookup-implementation

This behavior can be encapsulated into a `PendingUtxos`
structure described below.

```rust
// sketch
#[derive(Default, Debug)]
struct PendingUtxos(HashMap<OutPoint, oneshot::Sender<Utxo>>);

impl PendingUtxos {
    // adds the outpoint and returns (wrapped) rx end of oneshot
    // checks the spend height and restriction before sending the utxo response
    // return can be converted to `Service::Future`
    pub fn queue(
        &mut self,
        outpoint: OutPoint,
        spend_height: Height,
        spend_restriction: SpendRestriction,
    ) -> impl Future<Output=Result<Response, ...>>;

    // if outpoint is a hashmap key, remove the entry and send output on the channel
    pub fn respond(&mut self, outpoint: OutPoint, output: transparent::Output);

    /// check the list of pending UTXO requests against the supplied `utxos`
    pub fn check_against(&mut self, utxos: &HashMap<transparent::OutPoint, Utxo>);

    // scans the hashmap and removes any entries with closed senders
    pub fn prune(&mut self);
}
```

The state service should maintain an `Arc<Mutex<PendingUtxos>>`, used as follows:

1. In `Service::call(Request::AwaitSpendableUtxo { outpoint: u, .. }`, the service should:

- call `PendingUtxos::queue(u)` to get a future `f` to return to the caller;
- spawn a task that does a rocksdb lookup for `u`, calling `PendingUtxos::respond(u, output)` if present;
- check the in-memory storage for `u`, calling `PendingUtxos::respond(u, output)` if present;
- return `f` to the caller (it may already be ready).
  The common case is that `u` references an old spendable UTXO, so spawning the lookup
  task first means that we don't wait to check in-memory storage for `u`
  before starting the rocksdb lookup.

1. In `f`, the future returned by `PendingUtxos::queue(u)`, the service should
   check that the `Utxo` is spendable before returning it:

- if `Utxo.from_coinbase` is false, return the utxo;
- if `Utxo.from_coinbase` is true, check that:
  - `spend_restriction` is `AllShieldedOutputs`, and
  - `spend_height` is greater than or equal to
    `MIN_TRANSPARENT_COINBASE_MATURITY` plus the `Utxo.height`,
  - if both checks pass, return the utxo.
  - if any check fails, drop the utxo, and let the request timeout.

1. In `Service::call(Request::CommitBlock(block, ..))`, the service should:

- [check for double-spends of each UTXO in the block](https://github.com/ZcashFoundation/zebra/issues/2231),
  and
- do any other transactional checks before committing a block as normal.
  Because the `AwaitSpendableUtxo` request is informational, there's no need to do
  the transactional checks before matching against pending UTXO requests,
  and doing so upfront can run expensive verification earlier than needed.

1. In `Service::poll_ready()`, the service should call
   `PendingUtxos::prune()` at least _some_ of the time. This is required because
   when a consumer uses a timeout layer, the cancelled requests should be
   flushed from the queue to avoid a resource leak. However, doing this on every
   call will result in us spending a bunch of time iterating over the hashmap.

# Drawbacks

[drawbacks]: #drawbacks

One drawback of this design is that we may have to wait on a lock. However,
the critical section basically amounts to a hash lookup and a channel send,
so I don't think that we're likely to run into problems with long contended
periods, and it's unlikely that we would get a deadlock.

# Rationale and alternatives

[rationale-and-alternatives]: #rationale-and-alternatives

High-level design rationale is inline with the design sketch. One low-level
option would be to avoid encapsulating behavior in the `PendingUtxos` and
just have an `Arc<Hashmap<..>>`, so that the lock only protects the hashmap
lookup and not sending through the channel. But I think the current design is
cleaner and the cost is probably not too large.

# Unresolved questions

[unresolved-questions]: #unresolved-questions

- We need to pick a timeout for UTXO lookup. This should be long enough to
  account for the fact that we may start verifying blocks before all of their
  ancestors are downloaded.

# Implementation Notes

[implementation-notes]: #implementation-notes

> **Last validated:** December 2025

This RFC has been fully implemented with evolved design patterns. The following
notes document the implementation details:

## Type Name Changes

| RFC Name | Implementation Name | Location |
|----------|-------------------|----------|
| `SpendRestriction` | `CoinbaseSpendRestriction` | `zebra-chain/src/transparent/utxo.rs:127-140` |
| `AwaitSpendableUtxo` | `AwaitUtxo` | `zebra-state/src/request.rs:906` |
| `Response::SpendableUtxo` | `Response::Utxo` | `zebra-state/src/response.rs:85-87` |

## Design Evolution

The request was simplified from `AwaitSpendableUtxo { outpoint, spend_height, spend_restriction }`
to just `AwaitUtxo(transparent::OutPoint)`. Spend restrictions are now handled
at the transaction verification layer rather than the state service layer.

## Core Components (Verified)

| Component | Location |
|-----------|----------|
| `CachedFfiTransaction` | `zebra-script/src/lib.rs:81-93` |
| `is_valid()` | `zebra-script/src/lib.rs:131-188` |
| `Utxo` struct | `zebra-chain/src/transparent/utxo.rs:17-28` |
| `PendingUtxos` | `zebra-state/src/service/pending_utxos.rs:11-73` |
| `transparent_coinbase_spend()` | `zebra-state/src/service/check/utxo.rs:190-217` |

## Timeout Configuration (Resolved)

| Timeout | Value | Location |
|---------|-------|----------|
| UTXO lookup | 6 minutes | `zebra-consensus/src/transaction.rs:62` |
| Mempool output lookup | 60 seconds | `zebra-consensus/src/transaction.rs:71` |

## Enhancements Beyond RFC

- Uses `broadcast::channel` instead of `oneshot::Sender` (supports multiple subscribers per UTXO)
- Separate `OrderedUtxo` type for tracking transaction indices
- `known_utxos` parameter for pre-computed UTXO sets with block verification
