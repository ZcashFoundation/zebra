# System Architecture

This section describes Zebra as a running system: how its components fit
together, how data and control flow between them, and why the design is
shaped the way it is. It is intended for developers who already understand
decentralized systems in general — what a block, transaction, chain fork,
and gossip protocol are — but do not need deep familiarity with the Zcash
or Bitcoin protocols.

The [Design Overview](overview.md) catalogs what each crate does. These
pages are the companion narrative: the mental model of how the pieces
cooperate at runtime, and the record of why notable design decisions were
made.

## What Zebra Is (and Is Not)

Zebra is a **validator full node** for Zcash. Its job is to stay synced
with the best chain, verify every block and transaction that reaches it
against the consensus rules, serve the resulting state to peers and local
clients, and relay newly observed data. It is not a wallet, block explorer,
indexer-for-applications, mining pool, or light-client backend. Those
responsibilities belong to sibling projects (Zaino, Zallet, librustzcash,
lightwalletd, and so on).

This narrow scope is intentional. A validator's correctness budget is
mostly spent on consensus, networking, and storage; mixing in
application-layer features historically inflated attack surface and
coupling in node implementations. Zebra treats everything outside
validation as someone else's concern.

Two related principles follow from that scope:

- **Library-first.** Each major responsibility lives in its own crate
  with a stable-ish boundary, so that downstream projects can reuse the
  network stack, chain types, or verifier without linking the full node.
- **Minimize trust, in both dependencies and inputs.** Untrusted bytes
  from peers, RPC clients, and disk are validated at the boundary they
  cross. Dependencies are kept small and well-scoped; duplicated crypto
  primitives are avoided in favor of the ecosystem's vetted crates.

## Reading Order

The pages in this section build on each other, but each is written to
stand alone:

- [The Crate Graph](architecture/crate-graph.md) — the workspace
  layout and the dependency rules that keep it disciplined.
- [Services and Backpressure](architecture/services.md) — the
  `tower::Service` abstraction that is Zebra's primary organizing
  principle.
- [Startup and Orchestration](architecture/startup.md) — how `zebrad`
  wires the services together.
- [Block Lifecycle](architecture/block-lifecycle.md) — how a block
  flows from peer receipt to the best chain, including the split
  between checkpoint and semantic verification.
- [State](architecture/state.md) — the read/write split and the
  finalized/non-finalized tiers.
- [Network](architecture/network.md) — per-peer state machines and
  the internal request/response protocol.
- [Sync](architecture/sync.md) — pipelined block lookup with hedging
  and retries.
- [Mempool](architecture/mempool.md) — when it activates and what it
  depends on.
- [RPC](architecture/rpc.md) — JSON-RPC, gRPC, and `zcashd`
  compatibility.
- [Concurrency Patterns](architecture/concurrency.md) — the recurring
  async patterns you will see throughout the codebase.
- [Design Decisions](architecture/design-decisions.md) — notable
  choices recorded in *decision / why / implications* form.

New design decisions that affect the shape of the system overall belong
on the [Design Decisions](architecture/design-decisions.md) page. Those
with deeper rationale, alternatives considered, or narrower scope
should be written up as an [RFC](rfcs.md) and linked from there.
