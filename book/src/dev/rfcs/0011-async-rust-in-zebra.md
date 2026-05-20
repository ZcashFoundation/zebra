- Feature Name: async_rust_in_zebra
- Start Date: 2021-03-30
- Design PR: [ZcashFoundation/zebra#1965](https://github.com/ZcashFoundation/zebra/pull/1965)
- Zebra Issue: [ZcashFoundation/zebra#1593](https://github.com/ZcashFoundation/zebra/issues/1593)

# Summary

[summary]: #summary

Zebra programmers need to carefully write async code so it doesn't deadlock or hang.
This is particularly important for `poll`, `select`, `Buffer`, `Batch`, and `Mutex`.

Zebra executes concurrent tasks using [async Rust](https://rust-lang.github.io/async-book/),
with the [tokio](https://docs.rs/tokio/) executor.

At a higher level, Zebra also uses [`tower::Service`s](https://docs.rs/tower/0.4.1/tower/trait.Service.html),
[`tower::Buffer`s](https://docs.rs/tower/0.4.1/tower/buffer/struct.Buffer.html),
and our own [`tower-batch-control`](https://github.com/ZcashFoundation/zebra/tree/main/tower-batch-control)
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
- `contention`: slower execution because multiple tasks are waiting to
  acquire a lock, buffer/batch slot, or readiness.
- `missed wakeup`: a task `hang`s because it is never scheduled for wakeup.
- `lock`: exclusive access to a shared resource. Locks stop other code from
  running until they are released. For example, a mutex, buffer slot,
  or service readiness.
- `critical section`: code that is executed while holding a lock.
- `deadlock`: a `hang` that stops an async task executing code, because it
  is waiting for a lock, slot, or task readiness.
  For example: a task is waiting for a service to be ready, but the
  service readiness depends on that task making progress.
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

To avoid missed wakeups, futures must schedule a wakeup before they return
`Poll::Pending`. For more details, see the [`Poll::Pending` and Wakeups](#poll-pending-and-wakeups)
section.

Zebra's [unready_service.rs](https://github.com/ZcashFoundation/zebra/blob/de6d1c93f3e4f9f4fd849176bea6b39ffc5b260f/zebra-network/src/peer_set/unready_service.rs#L43) uses the `ready!` macro to correctly handle
`Poll::Pending` from the inner service.

You can see some similar constraints in
[pull request #1954](https://github.com/ZcashFoundation/zebra/pull/1954).

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

## Futures-Aware Mutexes

[futures-aware-mutexes]: #futures-aware-mutexes

To avoid hangs or slowdowns, prefer futures-aware types,
particularly for complex waiting or locking code.
But in some simple cases, [std::sync::Mutex is more efficient](https://docs.rs/tokio/1.15.0/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use).
For more details, see the [Futures-Aware Types](#futures-aware-types) section.

Zebra's [`Handshake`](https://github.com/ZcashFoundation/zebra/blob/a63c2e8c40fa847a86d00c754fb10a4729ba34e5/zebra-network/src/peer/handshake.rs#L204)
won't block other tasks on its thread, because it uses `futures::lock::Mutex`:

<!-- copied from commit a63c2e8c40fa847a86d00c754fb10a4729ba34e5 on 2020-04-30 -->

```rust
pub async fn negotiate_version(
    peer_conn: &mut Framed<TcpStream, Codec>,
    addr: &SocketAddr,
    config: Config,
    nonces: Arc<futures::lock::Mutex<HashSet<Nonce>>>,
    user_agent: String,
    our_services: PeerServices,
    relay: bool,
) -> Result<(Version, PeerServices), HandshakeError> {
    // Create a random nonce for this connection
    let local_nonce = Nonce::default();
    // # Correctness
    //
    // It is ok to wait for the lock here, because handshakes have a short
    // timeout, and the async mutex will be released when the task times
    // out.
    nonces.lock().await.insert(local_nonce);

    ...
}
```

Zebra's [`Inbound service`](https://github.com/ZcashFoundation/zebra/blob/0203d1475a95e90eb6fd7c4101caa26aeddece5b/zebrad/src/components/inbound.rs#L238)
can't use an async-aware mutex for its `AddressBook`, because the mutex is shared
with non-async code. It only holds the mutex to clone the address book, reducing
the amount of time that other tasks on its thread are blocked:

<!-- copied from commit 0203d1475a95e90eb6fd7c4101caa26aeddece5b on 2020-04-30 -->

```rust
// # Correctness
//
// Briefly hold the address book threaded mutex while
// cloning the address book. Then sanitize after releasing
// the lock.
let peers = address_book.lock().unwrap().clone();
let mut peers = peers.sanitized();
```

## Avoiding Deadlocks when Acquiring Buffer or Service Readiness

[readiness-deadlock-avoidance]: #readiness-deadlock-avoidance

To avoid deadlocks, readiness and locks must be acquired in a consistent order.
For more details, see the [Acquiring Buffer Slots, Mutexes, or Readiness](#acquiring-buffer-slots-mutexes-readiness)
section.

Zebra's [`ChainVerifier`](https://github.com/ZcashFoundation/zebra/blob/3af57ece7ae5d43cfbcb6a9215433705aad70b80/zebra-consensus/src/chain.rs#L73)
avoids deadlocks, contention, and errors by:

- calling `poll_ready` before each `call`
- acquiring buffer slots for the earlier verifier first (based on blockchain order)
- ensuring that buffers are large enough for concurrent tasks

<!-- This fix was in https://github.com/ZcashFoundation/zebra/pull/1735 , but it's
a partial revert, so the PR is a bit confusing. -->

<!-- edited from commit 306fa882148382299c8c31768d5360c0fa23c4d0 on 2020-04-07 -->

```rust
// We acquire checkpoint readiness before block readiness, to avoid an unlikely
// hang during the checkpoint to block verifier transition. If the checkpoint and
// block verifiers are contending for the same buffer/batch, we want the checkpoint
// verifier to win, so that checkpoint verification completes, and block verification
// can start. (Buffers and batches have multiple slots, so this contention is unlikely.)
//
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

## Critical Section Compiler Errors

[critical-section-compiler-errors]: #critical-section-compiler-errors

To avoid deadlocks or slowdowns, critical sections should be as short as
possible, and they should not depend on any other tasks.
For more details, see the [Acquiring Buffer Slots, Mutexes, or Readiness](#acquiring-buffer-slots-mutexes-readiness)
section.

Zebra's [`CandidateSet`](https://github.com/ZcashFoundation/zebra/blob/0203d1475a95e90eb6fd7c4101caa26aeddece5b/zebra-network/src/peer_set/candidate_set.rs#L257)
must release a `std::sync::Mutex` lock before awaiting a `tokio::time::Sleep`
future. This ensures that the threaded mutex lock isn't held over the `await`
point.

If the lock isn't dropped, compilation fails, because the mutex lock can't be
sent between threads.

<!-- edited from commit 0203d1475a95e90eb6fd7c4101caa26aeddece5b on 2020-04-30 -->

```rust
// # Correctness
//
// In this critical section, we hold the address mutex, blocking the
// current thread, and all async tasks scheduled on that thread.
//
// To avoid deadlocks, the critical section:
// - must not acquire any other locks
// - must not await any futures
//
// To avoid hangs, any computation in the critical section should
// be kept to a minimum.
let reconnect = {
    let mut guard = self.address_book.lock().unwrap();
    ...
    let reconnect = guard.reconnection_peers().next()?;

    let reconnect = MetaAddr::new_reconnect(&reconnect.addr, &reconnect.services);
    guard.update(reconnect);
    reconnect
};

// SECURITY: rate-limit new candidate connections
sleep.await;
```

## Sharing Progress between Multiple Futures

[progress-multiple-futures]: #progress-multiple-futures

To avoid starvation and deadlocks, tasks that depend on multiple futures
should make progress on all of those futures. This is particularly
important for tasks that depend on their own outputs. For more details,
see the [Unbiased Selection](#unbiased-selection)
section.

Zebra's [peer crawler task](https://github.com/ZcashFoundation/zebra/blob/375c8d8700764534871f02d2d44f847526179dab/zebra-network/src/peer_set/initialize.rs#L326)
avoids starvation and deadlocks by:

- sharing progress between any ready futures using the `select!` macro
- spawning independent tasks to avoid hangs (see [Acquiring Buffer Slots, Mutexes, or Readiness](#acquiring-buffer-slots-mutexes-readiness))
- using timeouts to avoid hangs

You can see a range of hang fixes in [pull request #1950](https://github.com/ZcashFoundation/zebra/pull/1950).

<!-- edited from commit 375c8d8700764534871f02d2d44f847526179dab on 2020-04-08 -->

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

## Prioritising Cancellation Futures

[prioritising-cancellation-futures]: #prioritising-cancellation-futures

To avoid starvation, cancellation futures must take priority over other futures,
if multiple futures are ready. For more details, see the [Biased Selection](#biased-selection)
section.

Zebra's [connection.rs](https://github.com/ZcashFoundation/zebra/blob/375c8d8700764534871f02d2d44f847526179dab/zebra-network/src/peer/connection.rs#L423)
avoids hangs by prioritising the cancel and timer futures over the peer
receiver future. Under heavy load, the peer receiver future could always
be ready with a new message, starving the cancel or timer futures.

You can see a range of hang fixes in [pull request #1950](https://github.com/ZcashFoundation/zebra/pull/1950).

<!-- copied from commit 375c8d8700764534871f02d2d44f847526179dab on 2020-04-08 -->

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

## Atomic Shutdown Flag

[atomic-shutdown-flag]: #atomic-shutdown-flag

As of April 2021, Zebra implements some shutdown checks using an atomic `bool`.

Zebra's [shutdown.rs](https://github.com/ZcashFoundation/zebra/blob/24bf952e982bde28eb384b211659159d46150f63/zebra-chain/src/shutdown.rs)
avoids data races and missed updates by using the strongest memory
ordering ([`SeqCst`](https://doc.rust-lang.org/nomicon/atomics.html#sequentially-consistent)).

We plan to replace this raw atomic code with a channel, see [#1678](https://github.com/ZcashFoundation/zebra/issues/1678).

<!-- edited from commit 24bf952e982bde28eb384b211659159d46150f63 on 2020-04-22 -->

```rust
/// A flag to indicate if Zebra is shutting down.
///
/// Initialized to `false` at startup.
pub static IS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Returns true if the application is shutting down.
pub fn is_shutting_down() -> bool {
    // ## Correctness:
    //
    // Since we're shutting down, and this is a one-time operation,
    // performance is not important. So we use the strongest memory
    // ordering.
    // https://doc.rust-lang.org/nomicon/atomics.html#sequentially-consistent
    IS_SHUTTING_DOWN.load(Ordering::SeqCst)
}

/// Sets the Zebra shutdown flag to `true`.
pub fn set_shutting_down() {
    IS_SHUTTING_DOWN.store(true, Ordering::SeqCst);
}
```

## Integration Testing Async Code

[integration-testing]: #integration-testing

Sometimes, it is difficult to unit test async code, because it has complex
dependencies. For more details, see the [Testing Async Code](#testing-async-code)
section.

[`zebrad`'s acceptance tests](https://github.com/ZcashFoundation/zebra/blob/5bf0a2954e9df3fad53ad57f6b3a673d9df47b9a/zebrad/tests/acceptance.rs#L699)
run short Zebra syncs on the Zcash mainnet or testnet. These acceptance tests
make sure that `zebrad` can:

- sync blocks using its async block download and verification pipeline
- cancel a sync
- reload disk state after a restart

These tests were introduced in [pull request #1193](https://github.com/ZcashFoundation/zebra/pull/1193).

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

## Instrumenting Async Functions

[instrumenting-async-functions]: #instrumenting-async-functions

Sometimes, it is difficult to debug async code, because there are many tasks
running concurrently. For more details, see the [Monitoring Async Code](#monitoring-async-code)
section.

Zebra runs instrumentation on some of its async function using `tracing`.
Here's an instrumentation example from Zebra's [sync block downloader](https://github.com/ZcashFoundation/zebra/blob/306fa882148382299c8c31768d5360c0fa23c4d0/zebrad/src/components/sync/downloads.rs#L128):

<!-- there is no original PR for this code, it has been changed a lot -->

<!-- copied from commit 306fa882148382299c8c31768d5360c0fa23c4d0 on 2020-04-08 -->

```rust
/// Queue a block for download and verification.
///
/// This method waits for the network to become ready, and returns an error
/// only if the network service fails. It returns immediately after queuing
/// the request.
#[instrument(level = "debug", skip(self), fields(%hash))]
pub async fn download_and_verify(&mut self, hash: block::Hash) -> Result<(), Report> {
    ...
}
```

## Tracing and Metrics in Async Functions

[tracing-metrics-async-functions]: #tracing-metrics-async-functions

Sometimes, it is difficult to monitor async code, because there are many tasks
running concurrently. For more details, see the [Monitoring Async Code](#monitoring-async-code)
section.

Zebra's [client requests](https://github.com/ZcashFoundation/zebra/blob/375c8d8700764534871f02d2d44f847526179dab/zebra-network/src/peer/connection.rs#L585)
are monitored via:

- trace and debug logs using `tracing` crate
- related work spans using the `tracing` crate
- counters using the `metrics` crate

<!-- there is no original PR for this code, it has been changed a lot -->

<!-- copied from commit 375c8d8700764534871f02d2d44f847526179dab on 2020-04-08 -->

```rust
/// Handle an incoming client request, possibly generating outgoing messages to the
/// remote peer.
///
/// NOTE: the caller should use .instrument(msg.span) to instrument the function.
async fn handle_client_request(&mut self, req: InProgressClientRequest) {
    trace!(?req.request);

    let InProgressClientRequest { request, tx, span } = req;

    if tx.is_canceled() {
        metrics::counter!("peer.canceled", 1);
        tracing::debug!("ignoring canceled request");
        return;
    }
    ...
}
```

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

The reference section contains in-depth information about concurrency in Zebra:

- [After an await, the rest of the Future might not be run](#cancellation-safe)
- [Task starvation](#starvation)
- [`Poll::Pending` and Wakeups](#poll-pending-and-wakeups)
- [Futures-Aware Types](#futures-aware-types)
- [Acquiring Buffer Slots, Mutexes, or Readiness](#acquiring-buffer-slots-mutexes-readiness)
- [Buffer and Batch](#buffer-and-batch)
  - [Buffered Services](#buffered-services)
    - [Choosing Buffer Bounds](#choosing-buffer-bounds)
- [Awaiting Multiple Futures](#awaiting-multiple-futures)
  - [Unbiased Selection](#unbiased-selection)
  - [Biased Selection](#biased-selection)
- [Replacing Atomics with Channels](#replacing-atomics)
- [Testing Async Code](#testing-async-code)
- [Monitoring Async Code](#monitoring-async-code)

Most Zebra designs or code changes will only touch on one or two of these areas.

## After an await, the rest of the Future might not be run

[cancellation-safe]: #cancellation-safe

> Futures can be "canceled" at any await point. Authors of futures must be aware that after an await, the code might not run.
> Futures might be polled to completion causing the code to work. But then many years later, the code is changed and the future might conditionally not be polled to completion which breaks things.
> The burden falls on the user of the future to poll to completion, and there is no way for the lib author to enforce this - they can only document this invariant.

<https://github.com/rust-lang/wg-async-foundations/blob/master/src/vision/submitted_stories/status_quo/alan_builds_a_cache.md#-frequently-asked-questions>

In particular, [`FutureExt::now_or_never`](https://docs.rs/futures/0.3.17/futures/future/trait.FutureExt.html#method.now_or_never):

- drops the future, and
- doesn't schedule the task for wakeups.

So even if the future or service passed to `now_or_never` is cloned,
the task won't be awoken when it is ready again.

## Task starvation

[starvation]: #starvation

Tokio [tasks are scheduled cooperatively](https://docs.rs/tokio/1.15.0/tokio/task/index.html#what-are-tasks):

> a task is allowed to run until it yields, indicating to the Tokio runtime’s scheduler
> that it cannot currently continue executing.
> When a task yields, the Tokio runtime switches to executing the next task.

If a task doesn't yield during a CPU-intensive operation, or a tight loop,
it can starve other tasks on the same thread. This can cause hangs or timeouts.

There are a few different ways to avoid task starvation:

- run the operation on another thread using [`spawn_blocking`](https://docs.rs/tokio/1.15.0/tokio/task/fn.spawn_blocking.html) or [`block_in_place`](https://docs.rs/tokio/1.15.0/tokio/task/fn.block_in_place.html)
- [manually yield using `yield_now`](https://docs.rs/tokio/1.15.0/tokio/task/fn.yield_now.html)

## `Poll::Pending` and Wakeups

[poll-pending-and-wakeups]: #poll-pending-and-wakeups

When returning `Poll::Pending`, `poll` functions must ensure that the task will be woken up when it is ready to make progress.

In most cases, the `poll` function calls another `poll` function that schedules the task for wakeup.

Any code that generates a new `Poll::Pending` should either have:

- a `CORRECTNESS` comment explaining how the task is scheduled for wakeup, or
- a wakeup implementation, with tests to ensure that the wakeup functions as expected.

Note: `poll` functions often have a qualifier, like `poll_ready` or `poll_next`.

## Futures-Aware Types

[futures-aware-types]: #futures-aware-types

Prefer futures-aware types in complex locking or waiting code,
rather than types which will block the current thread.

For example:

- Use `futures::lock::Mutex` rather than `std::sync::Mutex`
- Use `tokio::time::{sleep, timeout}` rather than `std::thread::sleep`

Always qualify ambiguous names like `Mutex` and `sleep`, so that it is obvious
when a call will block.

If you are unable to use futures-aware types:

- block the thread for as short a time as possible
- document the correctness of each blocking call
- consider re-designing the code to use `tower::Services`, or other futures-aware types

In some simple cases, `std::sync::Mutex` is correct and more efficient, when:

- the value behind the mutex is just data, and
- the locking behaviour is simple.

In these cases:

> wrap the `Arc<Mutex<...>>` in a struct
> that provides non-async methods for performing operations on the data within,
> and only lock the mutex inside these methods

For more details, see [the tokio documentation](https://docs.rs/tokio/1.15.0/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use).

## Acquiring Buffer Slots, Mutexes, or Readiness

[acquiring-buffer-slots-mutexes-readiness]: #acquiring-buffer-slots-mutexes-readiness

Ideally, buffer slots, mutexes, or readiness should be:

- acquired with one lock per critical section, and
- held for as short a time as possible.

If multiple locks are required for a critical section, acquire them in the same
order any time those locks are used. If tasks acquire multiple locks in different
orders, they can deadlock, each holding a lock that the other needs.

If a buffer, mutex, future or service has complex readiness dependencies,
schedule those dependencies separate tasks using `tokio::spawn`. Otherwise,
it might deadlock due to a dependency loop within a single executor task.

Carefully read the documentation of the channel methods you call, to check if they lock.
For example, [`tokio::sync::watch::Receiver::borrow`](https://docs.rs/tokio/1.15.0/tokio/sync/watch/struct.Receiver.html#method.borrow)
holds a read lock, so the borrowed data should always be cloned.
Use `Arc` for efficient clones if needed.

Never have two active watch borrow guards in the same scope, because that can cause a deadlock. The
`watch::Sender` may start acquiring a write lock while the first borrow guard is active but the
second one isn't. That means that the first read lock was acquired, but the second never will be
because starting to acquire the write lock blocks any other read locks from being acquired. At the
same time, the write lock will also never finish acquiring, because it waits for all read locks to
be released, and the first read lock won't be released before the second read lock is acquired.

In all of these cases:

- make critical sections as short as possible, and
- do not depend on other tasks or locks inside the critical section.

### Acquiring Service Readiness

Note: do not call `poll_ready` on multiple tasks, then match against the results. Use the `ready!`
macro instead, to acquire service readiness in a consistent order.

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

We also avoid hangs because:

- the timeouts on network messages, block downloads, and block verification will restart verification if it hangs
- `Buffer` and `Batch` release their reservations when response future is returned by the buffered/batched service, even if the returned future hangs
  - in general, we should move as much work into futures as possible, unless the design requires sequential `call`s
- larger `Buffer`/`Batch` bounds

### Buffered Services

[buffered-services]: #buffered-services

A service should be provided wrapped in a `Buffer` if:

- it is a complex service
- it has multiple callers, or
- it has a single caller that calls it multiple times concurrently.

Services might also have other reasons for using a `Buffer`. These reasons should be documented.

#### Choosing Buffer Bounds

[choosing-buffer-bounds]: #choosing-buffer-bounds

Zebra's `Buffer` bounds should be set to the maximum number of concurrent requests, plus 1:

> it's advisable to set bound to be at least the maximum number of concurrent requests the `Buffer` will see
> <https://docs.rs/tower/0.4.3/tower/buffer/struct.Buffer.html#method.new>

The extra slot protects us from future changes that add an extra caller, or extra concurrency.

As a general rule, Zebra `Buffer`s should all have at least 5 slots, because most Zebra services can
be called concurrently by:

- the sync service,
- the inbound service, and
- multiple concurrent `zebra-client` blockchain scanning tasks.

Services might also have other reasons for a larger bound. These reasons should be documented.

We should limit `Buffer` lengths for services whose requests or responses contain `Block`s (or other large
data items, such as `Transaction` vectors). A long `Buffer` full of `Block`s can significantly increase memory
usage.

For example, parsing a malicious 2 MB block can take up to 12 MB of RAM. So a 5 slot buffer can use 60 MB
of RAM.

Long `Buffer`s can also increase request latency. Latency isn't a concern for Zebra's core use case as a node
software, but it might be an issue if wallets, exchanges, or block explorers want to use Zebra.

## Awaiting Multiple Futures

[awaiting-multiple-futures]: #awaiting-multiple-futures

When awaiting multiple futures, Zebra can use biased or unbiased selection.

Typically, we prefer unbiased selection, so that if multiple futures are ready,
they each have a chance of completing. But if one of the futures needs to take
priority (for example, cancellation), you might want to use biased selection.

### Unbiased Selection

[unbiased-selection]: #unbiased-selection

The [`futures::select!`](https://docs.rs/futures/0.3.13/futures/macro.select.html) and
[`tokio::select!`](https://docs.rs/tokio/0.3.6/tokio/macro.select.html) macros select
ready arguments at random by default.

To poll a `select!` in order, pass `biased;` as the first argument to the macro.

Also consider the `FuturesUnordered` stream for unbiased selection of a large number
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

- put shutdown or cancel oneshots first, then timers, then other futures
- use the `select!` macro to ensure fairness

Select's bias can be useful to ensure that cancel oneshots and timers are always
executed first. Consider the `select_biased!` macro and `FuturesOrdered` stream
for guaranteed ordered selection of futures. (However, this macro and stream require
mapping all arguments to the same type.)

The `futures::select` `Either` return type is complex, particularly when nested. This
makes code hard to read and maintain. Map the `Either` to a custom enum.

## Replacing Atomics with Channels

[replacing-atomics]: #replacing-atomics
[using-atomics]: #using-atomics

If you're considering using atomics, prefer a safe, tested, portable abstraction,
like tokio's [watch](https://docs.rs/tokio/*/tokio/sync/watch/index.html) or
[oneshot](https://docs.rs/tokio/*/tokio/sync/oneshot/index.html) channels.

In Zebra, we try to use safe abstractions, and write obviously correct code. It takes
a lot of effort to write, test, and maintain low-level code. Almost all of our
performance-critical code is in cryptographic libraries. And our biggest performance
gains from those libraries come from async batch cryptography.

We are [gradually replacing atomics with channels](https://github.com/ZcashFoundation/zebra/issues/2268)
in Zebra.

### Atomic Risks

[atomic-risks]: #atomic-risks
[atomic-details]: #atomic-details

Some atomic sizes and atomic operations [are not available on some platforms](https://doc.rust-lang.org/std/sync/atomic/#portability).
Others come with a performance penalty on some platforms.

It's also easy to use a memory ordering that's too weak. Future code changes might require
a stronger memory ordering. But it's hard to test for these kinds of memory ordering bugs.

Some memory ordering bugs can only be discovered on non-x86 platforms. And when they do occur,
they can be rare. x86 processors [guarantee strong orderings, even for `Relaxed` accesses](https://stackoverflow.com/questions/10537810/memory-ordering-restrictions-on-x86-architecture#18512212).
Since Zebra's CI all runs on x86 (as of June 2021), our tests get `AcqRel` orderings, even
when we specify `Relaxed`. But ARM processors like the Apple M1
[implement weaker memory orderings, including genuinely `Relaxed` access](https://stackoverflow.com/questions/59089084/loads-and-stores-reordering-on-arm#59089757). For more details, see the [hardware reordering](https://doc.rust-lang.org/nomicon/atomics.html#hardware-reordering)
section of the Rust nomicon.

But if a Zebra feature requires atomics:

1. use an `AtomicUsize` with the strongest memory ordering ([`SeqCst`](https://doc.rust-lang.org/nomicon/atomics.html#sequentially-consistent))
2. use a weaker memory ordering, with:

- a correctness comment,
- multithreaded tests with a concurrency permutation harness like [loom](https://github.com/tokio-rs/loom), on x86 and ARM, and
- benchmarks to prove that the low-level code is faster.

Tokio's watch channel [uses `SeqCst` for reads and writes](https://docs.rs/tokio/1.6.1/src/tokio/sync/watch.rs.html#286)
to its internal "version" atomic. So Zebra should do the same.

## Testing Async Code

[testing-async-code]: #testing-async-code

Zebra's existing acceptance and integration tests will catch most hangs and deadlocks.

Some tests are only run after merging to `main`. If a recently merged PR fails on `main`,
we revert the PR, and fix the failure.

Some concurrency bugs only happen intermittently. Zebra developers should run regular
full syncs to ensure that their code doesn't cause intermittent hangs. This is
particularly important for code that modifies Zebra's highly concurrent crates:

- `zebrad`
- `zebra-network`
- `zebra-state`
- `zebra-consensus`
- `tower-batch-control`
- `tower-fallback`

## Monitoring Async Code

[monitoring-async-code]: #monitoring-async-code

Zebra uses the following crates for monitoring and diagnostics:

- [tracing](https://tracing.rs/tracing/): tracing events, spans, logging
- [tracing-futures](https://docs.rs/tracing-futures/): future and async function instrumentation
- [metrics](https://docs.rs/metrics-core/) with a [prometheus exporter](https://docs.rs/metrics-exporter-prometheus/)

These introspection tools are also useful during testing:

- `tracing` logs individual events
  - spans track related work through the download and verification pipeline
- `metrics` monitors overall progress and error rates
  - labels split counters or gauges into different categories (for example, by peer address)

# Drawbacks

[drawbacks]: #drawbacks

Implementing and reviewing these constraints creates extra work for developers.
But concurrency bugs slow down every developer, and impact users. And diagnosing
those bugs can take a lot of developer effort.

# Unresolved questions

[unresolved-questions]: #unresolved-questions

Can we catch these bugs using automated tests?

How can we diagnose these kinds of issues faster and more reliably?

- [TurboWish](https://blog.pnkfx.org/blog/2021/04/26/road-to-turbowish-part-1-goals/)
  (also known as `tokio-console`)
  looks really promising for task, channel, and future introspection. As of May 2020,
  there is an [early prototype available](https://github.com/tokio-rs/console).
