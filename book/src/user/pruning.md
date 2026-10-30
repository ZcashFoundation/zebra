# Pruned mining nodes

Pruning saves disk space on mining and pool backends by deleting old finalized transaction bodies.
Validation state, current UTXOs, headers, transaction IDs, address indexes, and shielded tree data
remain available. Mining, block submission, and spending old unspent outputs still work. Initial
sync still downloads and validates the chain; pruning does not skip validation.

Use an archive node instead for historical wallet rescans, indexer bootstrap, or serving full block
history to peers. Both zcashd sidecar modes require an archive and are rejected when pruning is
configured, even before deletion. A pool may also need an archive for payout and accounting queries.

**Deletion is irreversible.** Keep an archive backup if you may need the history again. Pruning does
not promise lower RAM use or a fixed database-size ceiling: retained indexes and validation state
keep growing. Allow free disk space for initial sync, snapshot extraction, and RocksDB compaction;
logical deletion does not immediately free disk space.

## Enable pruning and choose retention

Add the following to `zebrad.toml`, or add the option to its existing `[state]` section,
then restart:

```toml
[state]
prune_blocks_older_than = 10000
```

The value is the number of recent **finalized** block bodies to retain, measured from the durable
finalized tip rather than the best chain tip. Genesis and all non-finalized blocks are
also retained.
The minimum is 10,000 on Mainnet and Testnet, including custom testnets, or 1,001 on Regtest
(`MAX_BLOCK_REORG_HEIGHT + 1`). Zero and smaller values are invalid. Omitting the setting selects
archive operation, the default; it cannot reopen a database whose history has already been pruned.

Choose the window for the queries your pool actually makes, not just for mining work. At 75-second
spacing, 10,000 blocks cover about 8.7 days; at 25-second spacing they cover about 2.9 days.
These are spacing estimates, not a time-based retention guarantee. Allow for payout delays, accounting
reconciliation, outages, and retries. Keep an archive backend for queries outside that window.

Increasing retention preserves more future history but never restores deleted bodies. A restored
pruned snapshot can therefore start with less history than the configured window requests.

## Startup and deletion

A single background worker waits for database migrations, spending-index conversion, and initial
format checks to finish successfully. Skipped, cancelled, or failed checks do not permit deletion.
The worker runs at startup and after finalized-tip changes, deleting at most 100 complete block
heights per batch. It yields between batches and finishes an in-flight batch before shutdown.
Normal RocksDB compaction reclaims the obsolete transaction data later, outside the block commit
path. Unexpected deletion failures are fatal, not silently retried.

Restart checks require retained evidence that the cached chain follows the configured network
upgrades. After NU5, a matching V5/V6 transaction provides that evidence; the check searches back at
most 100,000 blocks and refuses to start if missing history prevents it from finding a witness.
Pruning never deletes the witness needed by this check. It can retain an older suffix than requested
until a newer witness appears, and does not begin before suitable NU5 evidence exists.

Keep the same `indexer` build-feature setting after pruning. Switching the spending-index layout in
either direction requires full history and is rejected on pruned state. Future migrations that need
old bodies also require an archive snapshot or resync; changing the retention setting cannot satisfy
them.

## RPC and peer behaviour

The `getblockchaininfo` response describes actual stored history, not the configuration:

- An archive reports `pruned: false` and omits `pruneheight`, including before the first deletion.
- After deletion, `pruned: true` accompanies `pruneheight`, the first height of the complete
  retained body suffix. Genesis remains available separately below it. This floor is derived
  from the first retained transaction row above genesis, not a separately persisted pruning marker.

For historical queries:

- Requests using `getblock` verbosity 1 still return the complete transaction-ID list. Headers,
  address balances, address transaction IDs, current UTXOs, treestates, and subtrees
  remain available.
- Requests using `getblock` verbosity 0, 2, or 3 fail with code `-32603` and
  `"Block not available (pruned data)"` when a needed body is missing. Verbosity 3 can fail
  even for a retained block if a spent output's parent transaction was pruned.
- Requests using `getrawtransaction` for a known pruned transaction fail with code `-32603` and
  `"Transaction not available (pruned data)"`. Unknown transactions still return `-5`; mempool and
  retained transactions remain available.
- Responses from `gettxout` still include the value, script, confirmations, and coinbase flag for an
  old unspent output, but `version` is `null` if its creating transaction was pruned. Unlike Zebra,
  zcashd stores the version in its coins database.
- The indexer and native lightwalletd gRPC services report `FAILED_PRECONDITION` for pruned bodies.
  A stream that encounters pruned history attempts to send a terminal error, but a stalled consumer
  can miss it when the send times out. Earlier results or stream closure alone do not prove that
  the requested range is complete.

See the [RPC migration guide](zcashd-api-migration-guide.md#pruned-history) and
[API history contract](../api.md#historical-data-availability) for compatibility details.

Enabling pruning removes `NODE_NETWORK` from peer-service advertisements immediately, before any
body is deleted. The node still syncs, relays, and serves retained data and historical headers, but
returns `notfound` for unavailable block bodies. Block inventory stops at missing history rather
than skipping the gap. Do not use this node as an archive source for other peers or a
zcashd sidecar.

## Watch pruning converge

Enable the [Prometheus endpoint](metrics.md) and watch these metrics:

- The `state.pruning.retained.height` gauge is the actual retained-body floor; zero means no missing
  historical prefix.
- The `state.pruning.deleted.height.count` counter counts block heights deleted by this process,
  not transactions or bytes.
- The `state.pruning.batch.duration_seconds` histogram measures successful deletion batches,
  including their safety checks, not compaction.

Prometheus replaces dots in these names with underscores. After a finalized block commit, use
`state.finalized.block.height` as the finalized tip. For retention `N`, the target floor is
`max(1, finalized_tip + 1 - N)`. The remaining deletion backlog is
`max(0, target_floor - max(1, retained_floor))`; there is no separate target or backlog metric.

The floor should approach that target as deletion catches up. A paused floor can mean startup
checks are still running or an older restart witness must be preserved. Check startup logs and
actual `getblockchaininfo` availability before treating it as stalled. Disk usage can lag logical
deletion while compaction runs, so also monitor free space and the existing RocksDB metrics.

## Format 30, recovery, and copies

This release uses database format 30 for both archive and pruned nodes. When no v30 database exists,
startup can move an existing v29 database inside the cache directory from
`state/v29/<network>/` to `state/v30/<network>/`. This is a move, not a backup, and happens
even with pruning disabled. An older binary looking in v29 will not find the moved state and may
start syncing again. Do not rename or symlink v30 state into an older format's path: the
directory boundary keeps older binaries from treating incomplete history as an archive.

Before upgrading or pruning, stop Zebra and keep any required archive backup outside the managed
cache directory. After deletion, simply removing the option makes writable startup fail. To regain
archive service, restore a compatible **archive** snapshot or fully resync in a separate cache
directory with pruning disabled. Do not overlay an archive onto pruned files or expect a larger
retention value to fill gaps. Follow the [snapshot guide](snapshots.md) for format, trust,
and restore requirements.

The `copy-state` command replays available blocks; it does not recreate deleted history. It rejects
a pruned target and any source range crossing missing bodies before opening the target for writes.
Copying only genesis is allowed; resuming into an existing archive target is possible if its tip
matches the source chain and the needed range is retained. Target pruning is disabled for the copy
and comparison, regardless of its config; normal startup applies that config afterward. Use a
stopped-node backup to preserve a pruned database rather than trying to rebuild one
with `copy-state`.
