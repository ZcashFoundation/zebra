- Feature Name: async_rust_constraints
- Start Date: 2021-03-30
- Design PR: [ZcashFoundation/zebra#1965](https://github.com/ZcashFoundation/zebra/pull/1965)
- Zebra Issue: [ZcashFoundation/zebra#1593](https://github.com/ZcashFoundation/zebra/issues/1593)

# Summary
[summary]: #summary

Zebra executes concurrent tasks using [async Rust](https://rust-lang.github.io/async-book/),
with the [tokio](https://docs.rs/tokio/0.3.6/tokio/index.html) executor.
At a higher level, Zebra also uses [`tower::Service`s](https://docs.rs/tower/0.4.1/tower/trait.Service.html),
[`tower::Buffer`s](https://docs.rs/tower/0.4.1/tower/buffer/struct.Buffer.html),
and our own [`tower-batch`](https://github.com/ZcashFoundation/zebra/tree/main/tower-batch) implementation.

Zebra programmers need to carefully write async code so it doesn't deadlock or hang.
This is particularly important for `poll`, `select`, `Buffer`, and `Batch`.

# Motivation
[motivation]: #motivation

Like all concurrent programming, Zebra's code needs to obey certain constraints
to avoid deadlocks and hangs. Unfortunately, Rust's tooling in these areas is
still developing, so we need to check these constraints during development,
reviews, and testing.

# Definitions
[definitions]: #definitions

<!--
Lay out explicit definitions of any terms that are newly introduced or which cause confusion during the RFC design process.
-->

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

<!--
Explain the proposal as if it was already included in the project and you were teaching it to another Zebra programmer. That generally means:

- Introducing new named concepts.
- Explaining the feature largely in terms of examples.
- Explaining how Zebra programmers should *think* about the feature, and how it should impact the way they use Zebra. It should explain the impact as concretely as possible.
- If applicable, provide sample error messages, deprecation warnings, migration guidance, or test strategies.
- If applicable, describe the differences between teaching this to existing Zebra programmers and new Zebra programmers.

For implementation-oriented RFCs (e.g. for Zebra internals), this section should focus on how Zebra contributors should think about the change, and give examples of its concrete impact. For policy RFCs, this section should provide an example-driven introduction to the policy, and explain its impact in concrete terms.
-->

TODO: complete the examples in this section

## `Poll::Pending` Deadlock Avoidance
[poll-pending-deadlock-avoidance]: #poll-pending-deadlock-avoidance

Here's a deadlock avoidance example from [#1735](https://github.com/ZcashFoundation/zebra/pull/1735):
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

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

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

## `Poll::Pending` and Wakeups
[poll-pending-and-wakeups]: #poll-pending-and-wakeups

When returning `Poll::Pending`, `poll` functions must ensure that the task will be woken up when it is ready to make progress.

In most cases, the `poll` function calls another `poll` function that schedules the task for wakeup.

Any code that generates a new `Poll::Pending` should either have:
* a `CORRECTNESS` comment explaining how the task is scheduled for wakeup, or
* a wakeup implementation, with tests to ensure that the wakeup functions as expected.

Note: `poll` functions often have a qualifier, like `poll_ready` or `poll_next`.

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

## Test Plan
[test-plan]: #test-plan

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
