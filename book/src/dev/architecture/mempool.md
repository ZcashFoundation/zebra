# Mempool

The mempool holds unmined transactions that have passed verification
and are candidates for block templates. It is only activated when the
node is close enough to the chain tip; while syncing historical blocks,
running a mempool would waste effort verifying transactions against a
stale view of the chain.

## Dependencies

- **State**, to resolve UTXOs and check for nullifier conflicts.
- **Transaction verifier**, which runs the mempool variant of
  transaction validation (no coinbases, different fee rules).
- **Peer set**, to receive and relay transactions.
- **Chain tip watch**, to evict transactions invalidated by new
  blocks.

## Cooperating Tasks

Internally the mempool runs several cooperating tasks:

- A **crawler** that periodically asks peers for unknown transactions.
- A **downloads** stream that verifies fetched transactions with a
  per-transaction timeout.
- A **queue checker** that promotes verified transactions into the
  storage.
- A **gossip task** that relays newly accepted transactions to peers.

## Storage and Eviction

Storage uses fee-based eviction under memory pressure — a transaction
is only evicted if a better-paying one would otherwise be rejected.
This keeps the mempool aligned with ZIP-317 fee policy and avoids the
pathology where a flood of minimum-fee transactions crowds out
higher-fee ones.

For a complete operational specification, see the
[Mempool Specification](../mempool-specification.md) and the
[Mempool Architecture diagram](../diagrams/mempool-architecture.md).
