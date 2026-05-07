# State: Two Tiers, Two Services

`zebra-state` splits along two orthogonal axes.

## Axis 1 — Write Path vs Read Path

- **`StateService`** owns writes. It receives `Request` variants that
  commit blocks, and it is the only component that mutates on-disk
  state. It also serves reads, but forwards most of them to the read
  service to avoid contending with writes.
- **`ReadStateService`** is a clonable read-only handle. It answers
  `ReadRequest` variants (`Block`, `Transaction`, `TipPoolValues`, and
  so on) against RocksDB snapshots, so long-running RPC queries do not
  block commits.

## Axis 2 — Finalized vs Non-Finalized

- **Non-finalized state** lives in RAM. It is a set of `Chain`
  structures, one per fork currently being tracked, sharing common
  history by structural sharing. Reorgs happen here: a losing fork
  gets pruned when a heavier sibling accumulates enough work.
- **Finalized state** is the single best chain below the reorg
  depth, stored in RocksDB column families (blocks, transactions,
  UTXO set, nullifier sets, Sapling/Orchard note commitment trees,
  history tree, and indexes).

A block commit first lands in the non-finalized state. When a block
becomes buried deeper than the maximum reorg depth, its ancestors at
that depth are atomically moved to RocksDB. Reorg logic therefore only
needs to consider the in-memory forest.

This split is what lets Zebra handle reorgs cheaply (fork trees are
small and in memory), keep queries fast (RocksDB snapshots serve
concurrent readers), and avoid the classic "database rollback"
problem that afflicts monolithic designs.

## Operational Details Worth Knowing

- **Queued blocks.** A block arriving before its parent is held in a
  pending queue until the parent commits, rather than being rejected.
  This prevents head-of-line blocking when the syncer receives blocks
  slightly out of order.
- **Database format versioning.** The on-disk format has an explicit
  version (`zebra-state/src/constants.rs`). Upgrades are either
  backward-compatible or require an explicit migration path; see
  [Upgrading the State Database](../state-db-upgrades.md).
- **Readers tolerate staleness.** The write service is the only source
  of truth for mutations; readers must tolerate seeing a slightly
  stale snapshot between the time a commit begins and the time a new
  snapshot is taken.
