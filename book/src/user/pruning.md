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

For a fresh cache with no existing state, add the setting below and start Zebra; the new
database is created directly in `state-pruned/v29/<network>`.

For an existing archive, perform this **stopped-node activation** first:

1. Leave pruning disabled and start the compatible writer. Finish ordinary database upgrades
   and startup checks; verify the expected tip and historical queries, and confirm
   `getblockchaininfo` reports `pruned: false` and omits `pruneheight`.
2. Stop Zebra and **every direct primary/secondary database reader**. Keep a complete
   stopped-node archive backup outside the managed cache.
3. Confirm the intended archive is `state/v29/<network>` and the destination
   `state-pruned/v29/<network>` does not exist. Create only the destination's parent
   directories, then move the complete network directory, including its `version` file.
   Never merge directories, overwrite another chain, or change the format-major number.
4. Add the setting below, start the writer, verify its tip and actual history availability,
   and only then reconnect compatible readers.

Startup refuses archive activation without this explicit move, before any normal major-version
reuse or body deletion. If both kinds already exist for the current or previous major, stop and
put the intended database in a separate cache root; do not choose by overwriting one.

Add the option to the existing `[state]` section in `zebrad.toml`:

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
  Block-range and address-transaction streams preserve terminal errors even when their data queue
  is full. A consumer that stalls while successful results are being queued receives
  `DEADLINE_EXCEEDED`, not successful end-of-stream. Check range completeness if the connection fails.

See the [RPC migration guide](zcashd-api-migration-guide.md#pruned-history) and
[API history contract](../api.md#historical-data-availability) for compatibility details.

Enabling pruning removes `NODE_NETWORK` from peer-service advertisements immediately, before any
body is deleted. The node still syncs, relays, and serves retained data and historical headers, but
returns `notfound` for unavailable block bodies. Block inventory stops at missing history rather
than skipping the gap. Do not use this node as an archive source for other peers or a
zcashd sidecar.
The `getnetworkinfo` response reports the same advertised service mask in `localservices`.
Pruned listeners remain discoverable through address gossip; syncing outbound handshakes still
require an archive peer.
Outside sync, outbound peers without `NODE_NETWORK` may occupy at most
`network.peerset_initial_target_size` connection slots (25 of the default 75 outbound slots).
The remaining slots are reserved for serving peers; inbound and isolated connections are exempt.
When reconnecting, Zebra prefers the first ready candidate that may serve blocks
(`NODE_NETWORK` or unknown services), falling back to known non-serving candidates only when
none are ready. Falling behind does not evict existing non-serving connections; reserved serving
capacity remains available, and new syncing outbound handshakes require serving peers.

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

## Database kinds, recovery, and copies

Both database kinds use format **29.0.0** and the normal linear upgrade sequence.
Archives that never opted in remain in `state/v29/<network>/`, preserving ordinary
rollback to Zebra 7.0.0. Existing archives require the explicit stopped-node activation
above before enabling pruning; startup never moves an archive automatically. Fresh pruning
nodes start in `state-pruned/v29/<network>` directly. Both kinds use the same record layout.

Removing the setting keeps using `state-pruned/v29/<network>/`. Writable startup
allows a complete database but refuses it after bodies have been deleted. Filesystem
lookup and permission errors abort selection rather than choosing another path.
Coexisting archive and pruned network directories for the current or previous major
cause an error with pruning either enabled or disabled, including for secondary readers.
Stop all users of the cache and put the intended database in a separate cache root;
never merge the directories or overwrite one with the other.

Keep required backups outside the managed cache. Changing `state.delete_old_database` only
controls old-version cleanup; neither setting performs the explicit activation move.
Format upgrades reuse the previous major within each kind; pruning reserves no second major.

### Direct readers and snapshots

External readers, including Zallet's and Zaino's Zebra backends, must use a compatible
`zebra-state` dependency and resolve the actual kind with `Config::state_database_kind(network)?`.
Build paths with `Config::db_path(kind, state_database_format_version_in_code().major, network)`;
the format helper alone cannot distinguish archive and pruned directories. A secondary follows
the archive before activation and the pruned directory afterward without moving either, creating
primary directories, or falling back to another database. Stop readers during activation and
reconnect them afterward; start the writer first if record migrations are needed.

Snapshots must preserve the database kind, format version, and network namespace.
Restore a pruned snapshot only under `state-pruned`, into an empty separate cache,
with pruning enabled and the same spending-index layout. Never unpack pruned state under
`state` or overlay either kind onto an existing database.

### Roll back an archive to Zebra 7.0.0

For an archive that has stayed in v29, no directory rename or restore is needed:

1. Stop Zebra and every direct database reader.
2. Remove `prune_blocks_older_than` from the configuration if present; Zebra 7.0.0 rejects this
   unsupported field.
3. Start Zebra 7.0.0 with the same cache directory, network parameters, and `indexer` build-feature
   setting. Verify the tip and historical block queries before restoring dependent services.

Starting the new binary again with pruning disabled reopens the same v29 database. Storage
compatibility does not make an older binary support newer consensus rules or changed custom-network
parameters.

### Return complete isolated state to the archive path

Use this checked procedure only after enabling pruning **without deleting any bodies**.
Disabling pruning alone does not move the database back or make it discoverable by Zebra 7.0.0:

1. Remove the pruning option and restart the **new** binary in archive mode. Startup must succeed;
   it refuses an already-pruned database. Confirm that `getblockchaininfo` reports `pruned: false`
   and omits `pruneheight`. This also prevents deletion racing the availability check.
2. Stop Zebra and **all** direct database readers before moving any files.
3. Keep a stopped-node backup outside the managed cache. Confirm that the destination
   `state/v29/<network>/` does not exist, then move the complete `state-pruned/v29/<network>/` directory
   there. Never merge directories, overwrite another chain, or use a symlink to bypass the boundary.
4. Remove the unsupported pruning field and start Zebra 7.0.0 with the same network parameters and
   `indexer` setting. Verify the tip and historical bodies before restarting compatible readers.

If both directories already exist, stop and resolve them separately; do not combine their contents.
Outside this checked never-pruned procedure, moving isolated state into the archive kind is unsafe.

### Recover after deletion

Once `getblockchaininfo` reports `pruned: true`, rollback by rename is forbidden.
Removing the pruning option makes writable startup fail; a coexisting archive is not a fallback.
To regain full history, restore a compatible **archive** snapshot or fully resync in a separate
cache directory with pruning disabled. Do not overlay an archive onto pruned files or expect
a larger retention value to fill gaps. Follow the [snapshot guide](snapshots.md) for format,
trust, and restore requirements.

### Copy state

The `copy-state` command replays available blocks; it does not recreate deleted history. It rejects
a pruned target and any source range crossing missing bodies before opening the target for writes.
Copying only genesis is allowed; resuming into an existing archive target is possible if its tip
matches the source chain and the needed range is retained. Target pruning is disabled for the copy
and comparison, regardless of its config. If the resulting target is an archive and its saved
configuration enables pruning, the stopped-node activation above is still required before
normal startup. Use a stopped-node backup to preserve a pruned database rather than trying
to rebuild one with `copy-state`.
