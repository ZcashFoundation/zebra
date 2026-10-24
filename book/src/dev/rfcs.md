# Zebra RFCs

Zebra requests for comments (RFCs) are historical designs, not current API
documentation.

New decisions use [architectural decision records (ADRs)](https://github.com/ZcashFoundation/zebra/tree/main/docs/decisions)
and the [decision log instructions](https://github.com/ZcashFoundation/zebra/blob/main/docs/decisions/README.md).

Each RFC starts with its Zebra 7.0.0 status, code location, and divergences.
**Implemented** means the core mechanism exists; **Partially implemented**
means proposed work remains absent; **Superseded** means another design or
process replaces it. Status describes the design, not its tracking issue.

## Published records

| RFC | Status | Summary |
| --- | --- | --- |
| [0000: Template](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/rfcs/0000-template.md) | Superseded | Historical proposal template; new decisions use the ADR template. |
| [0001: Pipelinable block syncing and lookup](rfcs/0001-pipelinable-block-lookup.md) | Implemented | Discover prospective chain tips while downloading and verifying blocks concurrently. |
| [0002: Parallel verification](rfcs/0002-parallel-verification.md) | Implemented | Separate structural, semantic, and contextual verification to defer dependencies. |
| [0003: Inventory tracking](rfcs/0003-inventory-tracking.md) | Implemented | Track peer inventory to route requests for recently advertised data. |
| [0004: Asynchronous script verification](rfcs/0004-asynchronous-script-verification.md) | Implemented | Await transparent outputs before verifying scripts, independently of block commit order. |
| [0005: State updates](rfcs/0005-state-updates.md) | Implemented | Keep non-finalized forks in memory and finalized state in RocksDB. |
| [0006: Contextual difficulty validation](rfcs/0006-contextual-difficulty.md) | Implemented | Validate difficulty adjustments against ancestor headers. |
| [0007: Treestate](rfcs/0007-treestate.md) | Implemented | Maintain shielded note commitment trees, anchors, and nullifiers. |
| [0009: Zebra client](rfcs/0009-zebra-client.md) | Superseded | Proposed integrated wallet and scanning components; external tools now own wallet functionality. |
| [0010: Version 5 transactions](rfcs/0010-v5-transaction.md) | Superseded | Proposed Zebra-owned transaction representations; production transactions now wrap librustzcash types. |
| [0011: Async Rust in Zebra](rfcs/0011-async-rust-in-zebra.md) | Partially implemented | Concurrency constraints remain relevant; the proposed shutdown-atomic replacement is absent. |
| [0012: Value pools](rfcs/0012-value-pools.md) | Implemented | Validate transaction value balances and persist nonnegative chain value pools. |

The published sequence has no RFC 0008. Drafts are not published decisions.
