- Feature Name: async_rust_constraints
- Start Date: 2021-03-30
- Design PR: [ZcashFoundation/zebra#1965](https://github.com/ZcashFoundation/zebra/pull/1965)
- Zebra Issue: [ZcashFoundation/zebra#1593](https://github.com/ZcashFoundation/zebra/issues/1593)

# Summary
[summary]: #summary

Zebra programmers need to carefully write async code so it doesn't deadlock or hang.
This is particularly important for `poll`, `select`, `Buffer`, `Batch`, and `Mutex`.

Zebra executes concurrent tasks using [async Rust](https://rust-lang.github.io/async-book/),
with the [tokio](https://docs.rs/tokio/0.3.6/tokio/index.html) executor.

At a higher level, Zebra also uses [`tower::Service`s](https://docs.rs/tower/0.4.1/tower/trait.Service.html),
[`tower::Buffer`s](https://docs.rs/tower/0.4.1/tower/buffer/struct.Buffer.html),
and our own [`tower-batch`](https://github.com/ZcashFoundation/zebra/tree/main/tower-batch)
implementation.

# Motivation
[motivation]: #motivation

Like all concurrent codebases, Zebra needs to obey certain constraints to avoid
hangs. Unfortunately, Rust's tooling in these areas is still developing. So
Zebra developers need to manually check these constraints during design,
development, reviews, and testing.

# Definitions
[definitions]: #definitions

- `hang`: a Zebra component stops making progress.
- `constraint`: a rule that Zebra must follow to prevent `hang`s.
- `CORRECTNESS comment`: the documentation for a `constraint` in Zebra's code.
- `task`: an async task can execute code independently of other tasks, using
        cooperative multitasking.
- `deadlock`: a `hang` that stops an async task executing code.
        For example: a task is never woken up.
- `starvation` or `livelock`: a `hang` that executes code, but doesn't do
        anything useful. For example: a loop never terminates.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

If you are designing, developing, or testing concurrent Zebra code, follow the
patterns in these examples to avoid hangs.

If you are reviewing concurrent Zebra designs or code, make sure that:
- it is clear how the design or code avoids hangs
- the design or code follows the patterns in these examples (as much as possible)
- the concurrency constraints and risks are documented

The [Reference](#reference-level-explanation) section contains in-depth
background information about Rust async concurrency in Zebra.

Here are some examples of concurrent designs and documentation in Zebra:

## Registering Wakeups Before Returning Poll::Pending
[wakeups-poll-pending]: #wakeups-poll-pending

Here's a wakeup correctness example from
[unready_service.rs](https://github.com/ZcashFoundation/zebra/blob/main/zebra-network/src/peer_set/unready_service.rs#L43):

<!-- copied from commit de6d1c93f3e4f9f4fd849176bea6b39ffc5b260f on 2020-04-07 -->
```rust
// CORRECTNESS
//
// The current task must be scheduled for wakeup every time we return
// `Poll::Pending`.
//
//`ready!` returns `Poll::Pending` when the service is unready, and
// the inner `poll_ready` schedules this task for wakeup.
//
// `cancel.poll` also schedules this task for wakeup if it is canceled.
let res = ready!(this
    .service
    .as_mut()
    .expect("poll after ready")
    .poll_ready(cx));
```

## Avoiding Deadlocks when Aquiring Buffer or Service Readiness
[readiness-deadlock-avoidance]: #readiness-deadlock-avoidance

Here's an example from [#1735](https://github.com/ZcashFoundation/zebra/pull/1735)
which covers:
- calling `poll_ready` before each `call`
- deadlock avoidance when acquiring buffer slots
- buffer bounds

<!-- copied from the main branch on 2020-04-07 -->
```rust
// We acquire checkpoint readiness before block readiness, to avoid an unlikely
// hang during the checkpoint to block verifier transition. If the checkpoint and
// block verifiers are contending for the same buffer/batch, we want the checkpoint
// verifier to win, so that checkpoint verification completes, and block verification
// can start. (Buffers and batches have multiple slots, so this contention is unlikely.)
use futures::ready;
// The chain verifier holds one slot in each verifier, for each concurrent task.
// Therefore, any shared buffers or batches polled by these verifiers should double
// their bounds. (For example, the state service buffer.)
ready!(self
    .checkpoint
    .poll_ready(cx)
    .map_err(VerifyChainError::Checkpoint))?;
ready!(self.block.poll_ready(cx).map_err(VerifyChainError::Block))?;
Poll::Ready(Ok(()))
```

## Prioritising Cancellation Futures
[prioritising-cancellation-futures]: #prioritising-cancellation-futures

Here's an example from [#1950](https://github.com/ZcashFoundation/zebra/pull/1950)
which shows biased future selection using the `select` function:

<!-- copied from commit b51070ed323de7a980cd89043947a2b0828e8bf8 on 2020-04-07 -->
```rust
// CORRECTNESS
//
// Currently, select prefers the first future if multiple
// futures are ready.
//
// If multiple futures are ready, we want the cancellation
// to take priority, then the timeout, then peer responses.
let cancel = future::select(tx.cancellation(), timer_ref);
match future::select(cancel, peer_rx.next()) {
    ...
}
```

## Sharing Progress between Multiple Futures
[progress-multiple-futures]: #progress-multiple-futures

Here's an example from [#1950](https://github.com/ZcashFoundation/zebra/pull/1950)
which shows:
- biased future selection using the `select!` macro
- deadlock avoidance with `Mutex` locks
- spawning independent tasks to avoid hangs
- using timeouts to avoid hangs

<!-- edited from commit b51070ed323de7a980cd89043947a2b0828e8bf8 on 2020-04-07 -->
```rust
// CORRECTNESS
//
// To avoid hangs and starvation, the crawler must:
// - spawn a separate task for each handshake, so they can make progress
//   independently (and avoid deadlocking each other)
// - use the `select!` macro for all actions, because the `select` function
//   is biased towards the first ready future

loop {
    let crawler_action = tokio::select! {
        a = handshakes.next() => a,
        a = crawl_timer.next() => a,
        _ = demand_rx.next() => {
            if let Some(candidate) = candidates.next().await {
                // candidates.next has a short delay, and briefly holds the address
                // book lock, so it shouldn't hang
                DemandHandshake { candidate }
            } else {
                DemandCrawl
            }
        }
    };

    match crawler_action {
        DemandHandshake { candidate } => {
            // spawn each handshake into an independent task, so it can make
            // progress independently of the crawls
            let hs_join =
                tokio::spawn(dial(candidate, connector.clone()));
            handshakes.push(Box::pin(hs_join));
        }
        DemandCrawl => {
            // update has timeouts, and briefly holds the address book
            // lock, so it shouldn't hang
            candidates.update().await?;
        }
        // handle handshake responses and the crawl timer
    }
}
```

## Integration Testing Async Code
[integration-testing]: #integration-testing

Here's an example from [`zebrad/tests/acceptance.rs`](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/acceptance.rs#L699)
which shows:
- tests for the async Zebra block download and verification pipeline
- cancellation tests
- reload tests after a restart

<!-- edited from commit 5bf0a2954e9df3fad53ad57f6b3a673d9df47b9a on 2020-04-07 -->
```rust
/// Test if `zebrad` can sync some larger checkpoints on mainnet.
#[test]
fn sync_large_checkpoints_mainnet() -> Result<()> {
    let reuse_tempdir = sync_until(
        LARGE_CHECKPOINT_TEST_HEIGHT,
        Mainnet,
        STOP_AT_HEIGHT_REGEX,
        LARGE_CHECKPOINT_TIMEOUT,
        None,
    )?;

    // if stopping corrupts the rocksdb database, zebrad might hang or crash here
    // if stopping does not write the rocksdb database to disk, Zebra will
    // sync, rather than stopping immediately at the configured height
    sync_until(
        (LARGE_CHECKPOINT_TEST_HEIGHT - 1).unwrap(),
        Mainnet,
        "previous state height is greater than the stop height",
        STOP_ON_LOAD_TIMEOUT,
        Some(reuse_tempdir),
    )?;

    Ok(())
}
```

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

The reference section contains in-depth information about concurrency in Zebra:
- [`Poll::Pending` and Wakeups](#poll-pending-and-wakeups)
- [Acquiring Buffer Slots or Mutexes](#acquiring-buffer-slots-or-mutexes)
- [Buffer and Batch](#buffer-and-batch)
  - [Buffered Services](#buffered-services)
    - [Choosing Buffer Bounds](#choosing-buffer-bounds)
- [Awaiting Multiple Futures](#awaiting-multiple-futures)
  - [Unbiased Selection](#unbiased-selection)
  - [Biased Selection](#biased-selection)
- [Testing Async Code](#testing-async-code)

Most Zebra designs or code changes will only touch on one or two of these areas.

## `Poll::Pending` and Wakeups
[poll-pending-and-wakeups]: #poll-pending-and-wakeups

When returning `Poll::Pending`, `poll` functions must ensure that the task will be woken up when it is ready to make progress.

In most cases, the `poll` function calls another `poll` function that schedules the task for wakeup.

Any code that generates a new `Poll::Pending` should either have:
* a `CORRECTNESS` comment explaining how the task is scheduled for wakeup, or
* a wakeup implementation, with tests to ensure that the wakeup functions as expected.

Note: `poll` functions often have a qualifier, like `poll_ready` or `poll_next`.

## Acquiring Buffer Slots or Mutexes
[acquiring-buffer-slots-or-mutexes]: #acquiring-buffer-slots-or-mutexes

Ideally, buffer slots or mutexes should be acquired one at a time, and held for as short a time
as possible.

If that's not possible, acquire slots and mutexs in the same order across cooperating tasks.
If tasks acquire the same slots and mutexes in different orders, they can deadlock, each holding
a lock that the other needs.

Note: do not call `poll_ready` on multiple tasks, then match against the results. Use the `ready!`
macro instead.

If a task is waiting for service readiness, and that service depends on other futures to become ready,
make sure those futures are scheduled in separate tasks using `tokio::spawn`.

## Buffer and Batch
[buffer-and-batch]: #buffer-and-batch

The constraints imposed by the `tower::Buffer` and `tower::Batch` implementations are:
1. `poll_ready` must be called **at least once** for each `call` 
2. Once we've reserved a buffer slot, we always get `Poll::Ready` from a buffer, regardless of the
   current readiness of the buffer or its underlying service
3. The `Buffer`/`Batch` capacity limits the number of concurrently waiting tasks. Once this limit
   is reached, further tasks will block, awaiting a free reservation.
4. Some tasks can depend on other tasks before they resolve. (For example: block validation.)
   If there are task dependencies, **the `Buffer`/`Batch` capacity must be larger than the
   maximum number of concurrently waiting tasks**, or Zebra could deadlock (hang).

These constraints are mitigated by:
- the timeouts on network messages, block downloads and block verification, which restart verification if it hangs
- the `Buffer`/`Batch` reservation release when response future is returned by the buffer/batch, even if the future doesn't complete
  - in general, we should move as much work into futures as possible, unless the design requires sequential `call`s
- larger `Buffer`/`Batch` bounds

### Buffered Services
[buffered-services]: #buffered-services

A service should be provided wrapped in a `Buffer` if:
* it is a complex service
* it has multiple callers, or
* it has a single caller than calls it multiple times concurrently.

Services might also have other reasons for using a `Buffer`. These reasons should be documented.

#### Choosing Buffer Bounds
[choosing-buffer-bounds]: #choosing-buffer-bounds

Zebra's `Buffer` bounds should be set to the maximum number of concurrent requests, plus 1:
> it's advisable to set bound to be at least the maximum number of concurrent requests the `Buffer` will see
https://docs.rs/tower/0.4.3/tower/buffer/struct.Buffer.html#method.new

The extra slot protects us from future changes that add an extra caller, or extra concurrency.

As a general rule, Zebra `Buffer`s should all have at least 3 slots (2 + 1), because most Zebra services can
be called concurrently by:
* the sync service, and
* the inbound service.

Services might also have other reasons for a larger bound. These reasons should be documented.

We should limit `Buffer` lengths for services whose requests or responses contain `Block`s (or other large
data items, such as `Transaction` vectors). A long `Buffer` full of `Block`s can significantly increase memory
usage.

Long `Buffer`s can also increase request latency. Latency isn't a concern for Zebra's core use case as a node
software, but it might be an issue if wallets, exchanges, or block explorers want to use Zebra.

## Awaiting Multiple Futures
[awaiting-multiple-futures]: #awaiting-multiple-futures

### Unbiased Selection
[unbiased-selection]: #unbiased-selection

The [`futures::select!`](https://docs.rs/futures/0.3.13/futures/macro.select.html) and
[`tokio::select!`](https://docs.rs/tokio/0.3.6/tokio/macro.select.html) macros select
ready arguments at random. 

Also consdier the `FuturesUnordered` stream for unbiased selection of a large number
of futures. However, this macro and stream require mapping all arguments to the same
type.

Consider mapping the returned type to a custom enum with module-specific names.

### Biased Selection
[biased-selection]: #biased-selection

The [`futures::select`](https://docs.rs/futures/0.3.13/futures/future/fn.select.html)
is biased towards its first argument. If the first argument is always ready, the second
argument will never be returned. (This behavior is not documented or guaranteed.) This
bias can cause starvation or hangs. Consider edge cases where queues are full, or there
are a lot of messages. If in doubt:
- put cancel oneshots first, then timers, then other futures
- use the `select!` macro to ensure fairness

Select's bias can be useful to ensure that cancel oneshots and timers are always
executed first. Consider the `select_biased!` macro and `FuturesOrdered` stream
for guaranteed ordered selection of futures. (However, this macro and stream require
mapping all arguments to the same type.)

The `futures::select` `Either` return type is complex, particularly when nested. This
makes code hard to read and maintain. Map the `Either` to a custom enum.

## Testing Async Code
[testing-async-code]: #testing-async-code

Zebra's existing acceptance and integration tests will catch most hangs and deadlocks.

Some tests are only run after merging to `main`. If a recently merged PR fails on `main`,
we revert the PR, and fix the failure.

Some concurrency bugs only happen intermittently. Zebra developers should run regular
full syncs to ensure that their code doesn't cause intermittent hangs. This is
particularly important for code that modifies Zebra's highly concurrent crates:
* `zebrad`
* `zebra-network`
* `zebra-state`
* `zebra-consensus`
* `tower-batch`
* `tower-fallback`

# Drawbacks
[drawbacks]: #drawbacks

Implementing and reviewing these constraints creates extra work for developers.
But concurrency bugs slow down every developer, and impact users. And diagnosing
those bugs can take a lot of developer effort.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

<!--
- What makes this design a good design?
- Is this design a good basis for later designs or implementations?
- What other designs have been considered and what is the rationale for not choosing them?
- What is the impact of not doing this?
-->

# Prior art
[prior-art]: #prior-art

<!--
Discuss prior art, both the good and the bad, in relation to this proposal.
A few examples of what this can include are:

- For community proposals: Is this done by some other community and what were their experiences with it?
- For other teams: What lessons can we learn from what other communities have done here?
- Papers: Are there any published papers or great posts that discuss this? If you have some relevant papers to refer to, this can serve as a more detailed theoretical background.

This section is intended to encourage you as an author to think about the lessons from other projects, to provide readers of your RFC with a fuller picture.
If there is no prior art, that is fine - your ideas are interesting to us whether they are brand new or if they are an adaptation from other projects.

Note that while precedent set by other projects is some motivation, it does not on its own motivate an RFC.
Please also take into consideration that Zebra sometimes intentionally diverges from common Zcash features and designs.
-->

# Unresolved questions
[unresolved-questions]: #unresolved-questions

Can we catch these bugs using automated tests?

How can we diagnose these kinds of issues faster and more reliably?

# Future possibilities
[future-possibilities]: #future-possibilities

<!--
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
-->
