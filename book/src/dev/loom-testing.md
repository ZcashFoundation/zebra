# Concurrency testing with loom

Zebra uses [loom] to check the synchronization protocols it builds on `std`
primitives. A loom model is a small test that loom runs once for every
interleaving its threads allow, under the C11 memory model rather than the
stronger one of the machine running it. It finds memory-ordering bugs that x86
CI never shows, interleaving bugs in lock and reference-count hand-offs, lost
wakeups and deadlocks, and it replays a failing schedule deterministically.

Loom only sees the types it replaces. It models `std` atomics and fences,
`Mutex`, `RwLock`, `Condvar`, `Arc`, `thread::{spawn, yield_now, park}`, an
unbounded `mpsc` stub, and statics declared with `loom::lazy_static!`. It
cannot see tokio: tokio compiles to `std` for its dependents whatever
configuration flag is set, so `watch`, `mpsc`, `oneshot`, `Semaphore`,
`Notify`, tasks, `select!` and timers are out of its reach. There is no time,
I/O, RocksDB or real OS thread inside a model, and a panic in any model thread
fails the whole model, so `resume_unwind` paths cannot be modelled.

## When a change needs a model

A change that adds or alters a protocol on `std` atomics, `Mutex`/`RwLock`,
`Arc` strong counts or OS-thread hand-offs adds or updates a model in that
module's `loom_tests.rs`, with a `#[should_panic]` negative control for each
memory-ordering claim. tokio channels, `watch`, `Semaphore` and actor tasks are
not loom material: keep `start_paused` tests for those.

## The harness

Models are built only with
`RUSTFLAGS="--cfg zebra_loom -C debug-assertions"`. The flag is `zebra_loom`,
not loom's usual `loom`: under a bare `--cfg loom`, tokio fails to compile
unless its `sync` feature happens to be enabled, and changes its scheduler
constants. The workspace `Cargo.toml` declares the flag in its `check-cfg` list
and the `loom` version in `[workspace.dependencies]`.

A crate with models has:

- a cfg-gated development dependency, after its `[dev-dependencies]` table:

  ```toml
  [target.'cfg(zebra_loom)'.dev-dependencies]
  loom = { workspace = true }
  ```

- a crate-private shim, `pub(crate) mod loom;` in `lib.rs` backed by
  `src/loom.rs`, which production code imports from, as in
  `use crate::loom::sync::atomic::{AtomicUsize, Ordering};`;
- its name in the `-p` lists of the command below, of both steps of the
  `run-loom-tests` job in [`tests-unit.yml`][tests-unit], and of the loom
  command in `AGENTS.md`. Today these are `zebra-consensus`, `zebra-state` and
  `zebra-network`.

The zebra-consensus shim, as it starts:

```rust
// The models rely on overflow checks: refuse a build that sets the cfg without them.
#[cfg(all(test, zebra_loom, not(debug_assertions)))]
compile_error!("loom models need RUSTFLAGS=\"--cfg zebra_loom -C debug-assertions\"");

#[cfg(all(test, zebra_loom))]
pub(crate) mod sync {
    pub(crate) mod atomic {
        pub(crate) use ::loom::sync::atomic::{AtomicUsize, Ordering};
    }
}

#[cfg(not(all(test, zebra_loom)))]
pub(crate) mod sync {
    pub(crate) mod atomic {
        pub(crate) use std::sync::atomic::{AtomicUsize, Ordering};
    }
}
```

Shim rules:

1. The loom half is gated on `all(test, zebra_loom)`. The `test` matters: only
   the crate's own unit-test build swaps types, so dependents, integration
   tests and release builds see `std` unchanged.
2. The `std` half re-exports exactly the types the crate imports. An unused
   re-export is an `unused_imports` warning, which CI denies.
3. Every shim starts with the `compile_error!` guard above.
4. `Arc` is never shimmed. `std::sync::Arc` around loom atomics works, and
   keeps public signatures the same in both builds. A model that wants
   reference-count branch points imports `::loom::sync::Arc` itself.
5. API differences are wrapped, never changed at call sites. Loom's
   `Mutex::lock()` returns a `LockResult`, so the zebra-consensus shim exposes a
   `parking_lot::Mutex` newtype with an infallible `lock()`.
6. Loom constructors are not `const`, so a `static` becomes a pair: the `std`
   half names `std::sync::LazyLock` in full, and the loom half uses
   `::loom::lazy_static! { static ref NAME: Type = value; }`.
7. No blanket `allow`s: both halves lint clean, with and without the flag.

A model goes in two commits: a `refactor` commit with the shim, the import
swap and any seam a stub needs, which leaves the release build unchanged, then
a `test` commit with the model and the development dependency.

## Writing a model

Declare the module next to the existing tests, and keep the name: the nextest
`loom` profile selects tests by the `loom_tests` module.

```rust
#[cfg(test)]
mod tests;

#[cfg(all(test, zebra_loom))]
mod loom_tests;
```

A model is a `#[test]` that calls `::loom::model(|| { … })`. A model that needs
a preemption bound builds `::loom::model::Builder`, sets `preemption_bound` in
code, and calls `check`; unlike `model`, `Builder::check` does not install the
`LOOM_LOG` subscriber.

```rust
let mut builder = ::loom::model::Builder::new();
builder.preemption_bound = Some(2);
builder.check(|| { /* ... */ });
```

Keep models small and deterministic:

- Use the main thread plus at most four spawned threads. The fifth `spawn`
  aborts the test process.
- Join every spawned thread before the closure returns. Loom drops
  `lazy_static!` values when the closure returns, and joining is what makes
  the final assertions mean anything.
- Call `::loom::thread::yield_now()` in every spin loop.
- Replace real work with stubs: no RocksDB, tokio runtime, `Instant::now()` or
  `rand`. Where production code cannot take a stub, add a seam in the
  `refactor` commit.
- Record what the threads saw in atomics, never a mutex: loom atomics created
  inside the model, or `std` atomics when the recorder must outlive one
  execution.
- Give each memory-ordering claim a negative control: the same model with the
  ordering or the protocol weakened, marked
  `#[should_panic(expected = "…")]`, asserting on the main thread after the
  joins so the message is the test's own.
- Run exhaustively, and record the iteration count in the `loom_tests.rs`
  module documentation. A model above about 10⁵ iterations or 10 seconds sets
  `preemption_bound = Some(2)` in code. Budget two lock-heavy workers, or three
  workers with a couple of atomic operations each.

`-C debug-assertions` turns on overflow checks, so plain integer arithmetic
such as `pending -= 1` panics with `attempt to subtract with overflow` instead
of wrapping. Atomic `fetch_add` and `fetch_sub` always wrap, so a model of an
atomic counter asserts its value.

The `RetainedBlockPermit` model in
`zebra-consensus/src/checkpoint/loom_tests.rs` is a complete example. The
checkpoint verifier admits blocks through `&mut self`, while commit tasks drop
their permits on other threads; the model drives the production admission
function, `RetainedBlockPermit::try_admit`, against two concurrent drops. Its
`admit` helper calls `try_admit` with a `LIMIT` of two and an eviction that
frees nothing, and `count` loads the counter:

```rust
#[test]
fn count_matches_live_permits_and_never_exceeds_limit() {
    ::loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let first = admit(&counter).expect("an empty count is below the limit");
        let second = admit(&counter).expect("one live permit is below the limit");
        assert!(admit(&counter).is_none(), "admission stops at the limit");

        let droppers = [first, second].map(|permit| thread::spawn(move || drop(permit)));
        let mut live = Vec::new();
        for _ in 0..LIMIT {
            if let Some(permit) = admit(&counter) {
                assert!(
                    count(&counter) <= LIMIT,
                    "admission never exceeds the limit"
                );
                live.push(permit);
            }
        }
        for dropper in droppers {
            dropper
                .join()
                .expect("a panicking model thread fails the model before the join");
        }

        assert_eq!(
            count(&counter),
            live.len(),
            "the count equals the live permits"
        );
    });
}
```

Its negative control, `two_admitters_overshoot_the_limit`, admits from two
threads at once, which `&mut self` rules out in production, and expects the
`admission never exceeds the limit` assertion to fail.

## Running models

Run every model, with the same crates and flags as CI:

```sh
RUSTFLAGS="--cfg zebra_loom -C debug-assertions" cargo nextest run --profile loom --release --lib \
  -p zebra-consensus -p zebra-state -p zebra-network
```

Run one model and print loom's iteration count:

```sh
RUSTFLAGS="--cfg zebra_loom -C debug-assertions" LOOM_LOG=loom::model=info cargo nextest run --profile loom --release \
  -p zebra-consensus -E 'test(/checkpoint::loom_tests::count_matches/)' --no-capture
```

- `--release`, because loom runs each model thousands of times.
- `-C debug-assertions`, for the overflow checks above. The shims'
  `compile_error!` guard rejects a build that sets the flag without it, and CI
  uses the same string, so local and CI builds behave the same.
- `--profile loom` selects the `loom_tests` modules only. Ordinary tests are
  not gated, and fail under the flag as soon as they touch a shimmed type.
- Without the flag, the command finds no models and nextest exits with
  `error: no tests to run`.

The first run builds those crates and their dependencies again under the new
flags; cargo keeps both builds, so switching back and forth recompiles nothing.

| Variable | Effect |
| --- | --- |
| `LOOM_LOG` | Logging filter, in `tracing` `EnvFilter` syntax. `loom::model=info` prints one `Completed in N iterations` line per model. A bare `info` or `trace` also logs every thread switch, gigabytes for a long model, so use it on one short model only. |
| `LOOM_LOCATION` | With `LOOM_LOG=trace`, adds the source location of every branch to the log. Slow. |
| `LOOM_CHECKPOINT_FILE` | Saves the exploration state to this file and resumes from it, to bisect a long failing model. |
| `LOOM_CHECKPOINT_INTERVAL` | Iterations between checkpoints, 20,000 by default. |
| `LOOM_MAX_PREEMPTIONS` | Default preemption bound. CI does not set it: models bound themselves in code, so local runs and CI explore the same space. |
| `LOOM_MAX_BRANCHES` | Branches allowed per execution, 1,000 by default. |
| `LOOM_MAX_DURATION` | Stops exploring after this many seconds, checked every checkpoint interval. |

## Reading failures

| Symptom | Meaning | Next step |
| --- | --- | --- |
| An assertion from a model | An interleaving violates the invariant | Rerun the model by name with `LOOM_LOG=trace LOOM_LOCATION=1` and `--no-capture`. Exploration is deterministic, so the run reaches the same failing execution. |
| `deadlock; threads = [...]` | Every model thread is blocked: a lost wakeup or a lock-order cycle | As above; the thread states show which lock or park each thread waits on. |
| `cannot access Loom execution state from outside a Loom model` | An ordinary test ran under the flag, or a model used a shimmed type outside `loom::model` | Run with `--profile loom`; otherwise gate the test with `#[cfg(not(zebra_loom))]`. |
| `Model exceeded maximum number of branches` | A spin loop without `yield_now`, or a model far over budget | Add `::loom::thread::yield_now()`, or bound the model in code. |
| `SIGABRT`, "thread caused non-unwinding panic" | A fifth thread was spawned | Use fewer threads. |
| `SIGABRT`, "panic in a destructor during cleanup", after a first panic | Unwinding from the first panic dropped loom state that panicked again, such as a thread that was never joined or a lock the panicking thread held | Read the first panic: it is the finding. Assert on the main thread after the joins, so that a negative control reports its own message. |
| "coroutine in thread … has overflowed its stack" | A large stack frame in a model thread, which gets 32 KiB by default | Spawn it with `::loom::thread::Builder::new().stack_size(n)`, where `n` counts words, not bytes. |
| nextest `TIMEOUT` after two minutes | The model outgrew its budget | Bound it in code; bisect it locally with `LOOM_CHECKPOINT_FILE`. |
| `attempt to subtract with overflow`, or add or multiply | A counter underflowed or wrapped | Fix the protocol, and keep the flag. |
| `error: loom models need RUSTFLAGS="--cfg zebra_loom -C debug-assertions"` at compile time | The flag was set without `-C debug-assertions` | Use the full `RUSTFLAGS` string. |
| `error: no tests to run` | The flag was not set | Add `RUSTFLAGS="--cfg zebra_loom -C debug-assertions"`. |

## Limits

- At most five threads, including the main one.
- Loom treats `SeqCst` roughly as `AcqRel`, does not explore load buffering,
  and keeps only the last seven stores of each atomic, so it can miss some
  weak-memory behavior and report false alarms on `SeqCst` code.
- Models have no time: `Condvar::wait_timeout` never times out, and code that
  reads the clock needs a seam.
- State spaces grow exponentially with threads and operations: three threads
  with three read-modify-write operations each already take millions of
  executions.

CI runs the models in the `run-loom-tests` job, described in
[continuous integration](continuous-integration.md#runners-and-build-caches).

[loom]: https://docs.rs/loom/0.7.2/loom/
[tests-unit]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/tests-unit.yml
