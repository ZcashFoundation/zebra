# The Crate Graph

Zebra is a Cargo workspace. The fundamental rule is that **dependencies
flow strictly downward**:

```text
zebrad                              // CLI, orchestration, long-running tasks
  ├── zebra-rpc                     // JSON-RPC + indexer gRPC
  ├── zebra-consensus               // semantic verification (async)
  │    └── zebra-script             // Bitcoin-style script via FFI
  ├── zebra-state                   // chain state (finalized + non-finalized)
  ├── zebra-network                 // P2P, peer set, internal protocol
  └── zebra-node-services           // shared trait aliases, no impls
       └── zebra-chain              // core types, sync-only, no tokio
```

Two conventions keep this graph disciplined:

1. **`zebra-chain` is sync-only.** It contains the consensus-critical
   types (blocks, transactions, proofs, addresses, serializers) and
   nothing that assumes an async runtime. This makes it reusable from
   synchronous contexts (CLI tools, offline validators, third-party
   libraries) and keeps parsing logic away from scheduling concerns.

2. **`zebra-node-services` defines trait aliases only.** When two
   higher crates need to refer to "a thing that looks like the state
   service" without depending on the whole `zebra-state` crate, the
   alias lives here. This breaks what would otherwise be dense
   cross-crate dependencies.

Utility crates (`tower-batch-control`, `tower-fallback`, `zebra-test`,
`zebra-utils`) sit outside the main hierarchy and are depended on as
needed.

For the crate-by-crate breakdown of responsibilities and exported
types, see the [Design Overview](../overview.md).
