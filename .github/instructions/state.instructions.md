---
applyTo: "zebra-state/**"
---

# Safety review rules: state and storage

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Keep one source of truth for state

Store each piece of state once and derive everything else from it. Keep derived flags and indexes consistent with their source data, and prune them along with it.

Look for: the same value written to two column families or tables; a cached or cloned copy of shared state read instead of the live source; a summary flag updated in one code path but not in the prune or rollback path; redundant arguments that repeat data the callee can derive

## Bump the DB format version on disk changes

For any change to on-disk layout, encoding, column families, or upgrade code, bump the state database version at the correct major, minor, or patch level, using a version never merged or cached before. Document new column families with tree, key, and value tables in the database design docs.

Look for: new or changed column families, key encodings, or value encodings with no version constant change; a version constant that matches one already on `main` or in CI caches; a major bump where appending fields to a forward-compatible record would do; new column families missing from the database design docs

## Read related chain state from one snapshot

Read the tip and every value that depends on it from a single consistent zebra-state snapshot or query, resolving tip-relative inputs once per request. Update shared records in one atomic write.

Look for: two separate state requests whose results are combined, such as tip height and then block data; multi-column-family reads that do not share one database snapshot; read-modify-write sequences across separate batches; a partial result returned when part of a stored object is missing

## Keep fork choice consistent across all forks

Make chain selection, pruning, and invalidation consider every retained fork in the non-finalized state, and remove invalidated blocks from every place that holds them. Prune discarded branches before publishing snapshots to watchers or readers.

Look for: invalidation that updates only the best chain; tie-break logic that could evict the selected chain; a snapshot sent to watchers or readers before discarded forks are pruned; replicas that choose a chain from different inputs than the writer

## Design formats for downgrade and forward compatibility

Make persisted formats something older binaries can either read safely or refuse cleanly, appending new fields to the end of records. Schedule format changes around releases that users might roll back to.

Look for: a new field inserted in the middle of a serialized record; parsers that reject trailing bytes from newer versions; a format change merged just before a planned rollback release; no downgrade path for a changed record
