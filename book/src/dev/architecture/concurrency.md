# Concurrency Patterns

A short taxonomy of the patterns you will keep seeing in the code.
When in doubt, favor these over ad-hoc `tokio::spawn` and
`Arc<Mutex<_>>`.

## Tower Middleware Stacks

The syncer wraps the network service in
`Hedge<ConcurrencyLimit<Retry<Timeout<Network>>>>`. The inbound
service is `LoadShed<Buffer<Timeout<Inbound>>>`. Policy is
declarative; the raw services stay simple.

When you need to change retry behavior, timeouts, or concurrency
windows, prefer editing the wiring layer over reaching into the
service implementation.

## `spawn_blocking` for Crypto

zk-SNARK and signature verification runs on the blocking thread pool
so that a batch of proofs does not stall the async runtime's worker
threads. This applies to Sapling and Orchard proofs, Groth16
verification, and any CPU-bound work heavy enough to starve the
scheduler.

## `tokio::sync::watch` for Shared Freshness

Current chain tip, sync status, and non-finalized state all use
watch channels: one producer, any number of consumers, each consumer
always gets the latest value without history. This is the right tool
whenever consumers only care about the current value, not the stream
of changes.

## `broadcast` for Fan-Out Events

Tip-changed events go to whichever subscribers (RPC long-poll,
mempool evictor, progress logger) care about them. Use `broadcast`
when every subscriber must see every event, not just the latest.

## `FuturesUnordered` for In-Flight Work

Per-block download and verification futures are pushed into a single
`FuturesUnordered` rather than spawned as independent tasks. This
keeps cancellation semantics simple (drop the collection, everything
stops) and avoids allocating a task per block.

## Batch Control via `tower-batch-control`

Contemporaneous cryptographic verification requests are grouped and
verified together, exploiting amortization opportunities in the
underlying libraries. New cryptographic verification paths should
plug into the batch control system rather than verifying items one
at a time.

## Fallback via `tower-fallback`

Used to cleanly transition between the checkpoint and semantic
verifiers during startup, when one verifier may be unavailable or
not yet initialized.

## Channel Choice Cheat Sheet

| Use case | Channel |
| --- | --- |
| One-shot handoff during init | `oneshot` |
| Many producers, many consumers | `mpsc` |
| Latest value, many subscribers | `watch` |
| Every event, many subscribers | `broadcast` |
| Backpressured request/response | `tower::Service` |

Shared mutable state behind `Arc<Mutex<T>>` is rarely the right
answer — if a channel or watch fits, use that instead.
