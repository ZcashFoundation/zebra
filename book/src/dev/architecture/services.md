# Services and Backpressure

The single most load-bearing architectural choice in Zebra is that
**every stateful component exposes a [`tower::Service`]**. A Tower
service is, in essence, a typed async function with backpressure:

```rust
trait Service<Request> {
    type Response;
    type Error;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
    fn call(&mut self, req: Request) -> impl Future<Output = Result<Response, Error>>;
}
```

Callers must call `poll_ready` until the service is ready before
issuing `call`. The service can refuse readiness to shed load, queue
work, or wait on a downstream. This gives Zebra in-process microservices
with the same ergonomics as a remote RPC boundary: a request enum, a
response enum, backpressure, timeouts, retries, and load shedding are
all composable middleware.

## Why This Matters

- **Explicit backpressure.** An overloaded verifier or state service
  stops accepting work instead of silently queuing unbounded requests.
  The syncer and inbound path then naturally slow down rather than
  running the node out of memory.
- **Composable middleware.** Timeouts, buffers, concurrency limits,
  hedging, retries, and load-shedding are all stacked as layers.
  Policy lives at the wiring site rather than being threaded through
  business logic.
- **Test isolation.** A service can be unit-tested by feeding it a
  mock downstream — no need to spin up a real database or network.
- **Future portability.** Because the contract is already
  request/response, replacing an in-process service with an RPC call
  to a sibling process is a mechanical change, not a rewrite.

## The Public Surface

Each crate defines its own `Request` and `Response` enums — for example
`zebra_state::Request`, `zebra_state::ReadRequest`,
`zebra_consensus::Request`, and `zebra_network::Request`. Those enums
are the stable public surface; internal types change freely behind
them.

When you need two components to communicate, prefer extending the
relevant Request/Response enum over adding a side channel. Shared
mutable state, global singletons, or direct method calls that bypass
the service boundary defeat the backpressure story and make the
component harder to test or relocate.

[`tower::Service`]: https://docs.rs/tower/latest/tower/trait.Service.html
