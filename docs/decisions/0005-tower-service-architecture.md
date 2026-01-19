---
status: accepted
date: 2020-01-15
story: https://github.com/ZcashFoundation/zebra/issues/416
---

# Tower Service Architecture for Internal Communication

## Context and Problem Statement

Zebra needs a pattern for communication between its major components (consensus verification, state management, networking, RPC). The pattern must support async operations, backpressure, and composability while remaining testable.

## Priorities & Constraints

- Async-first design for network and I/O operations
- Backpressure propagation to prevent resource exhaustion
- Composability for middleware (batching, retries, timeouts)
- Testability with mock implementations
- Type safety for request/response protocols

## Considered Options

- Option 1: Tower Service trait
- Option 2: Actor model (e.g., Actix)
- Option 3: Direct async function calls
- Option 4: Channel-based message passing

### Pros and Cons of the Options

#### Option 1: Tower Service

- Good, because it provides built-in backpressure via `poll_ready`
- Good, because the ecosystem has many compatible middleware crates
- Good, because it's composable (services can wrap other services)
- Good, because request/response types are statically typed
- Bad, because it has a learning curve for new contributors

#### Option 2: Actor Model

- Good, because it provides isolation between components
- Good, because it's familiar to Erlang/Akka developers
- Bad, because it introduces runtime overhead for message routing
- Bad, because Rust's actor ecosystem was less mature at the time

#### Option 3: Direct Async Functions

- Good, because it's simple and familiar
- Bad, because no built-in backpressure mechanism
- Bad, because middleware requires manual implementation

#### Option 4: Channel-Based

- Good, because it's simple to understand
- Bad, because types are often erased at channel boundaries
- Bad, because backpressure requires manual implementation

## Decision Outcome

Chosen option: **Option 1: Tower Service**

Tower provides the right abstractions for Zebra's needs:

- `Service<Request>` trait defines the interface between components
- `poll_ready` enables natural backpressure propagation
- Middleware can be applied uniformly (batching, timeouts, retries)
- Request/Response enums provide type-safe internal protocols

### Expected Consequences

- All major components (state, consensus, network, RPC) implement `Service`
- Internal protocols use typed `Request` and `Response` enums
- Batch verification uses `tower-batch-control` middleware
- Tests can substitute mock services for isolation
- Contributors need to understand Tower patterns

## More Information

- [Tower Documentation](https://docs.rs/tower/)
- [Blog: A New Network Stack for Zcash](https://www.zfnd.org/blog/a-new-network-stack-for-zcash/)
- [Blog: Composable Futures-based Batch Verification](https://www.zfnd.org/blog/futures-batch-verification/)
