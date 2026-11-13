# Startup and sync

An open listener does not establish readiness; use the
[health checks](health.md). Startup tasks run concurrently, so log messages
interleave. The examples omit timestamps, tracing spans, and variable fields.
See [tracing](tracing.md) for filters and [metrics](metrics.md) for dashboards.

## Load configuration and diagnostics

Zebra loads [configuration](config.md), applies command-line overrides, and
initializes logging and the CPU thread pool. It logs the build version,
network, compiled features, state format, and loaded configuration.

Without a configuration file, the log includes:

```text
No config file provided, using default configuration
initialized rayon thread pool for CPU-bound tasks
```

Environment overrides still apply when this message says “default
configuration.” With a file, Zebra instead logs `Using config file at:` and
the selected path. Configured metrics and tracing-filter reload listeners
start during this application initialization, before the node state opens.

The startup command then logs:

```text
Starting zebrad
```

On Linux, it checks the TCP slow-start-after-idle setting and warns if it is
enabled. See [Linux TCP tuning](troubleshooting.md#linux-tcp-tuning-for-block-propagation).
In [zcashd-compat mode](zcashd-compat.md), Zebra performs its preflight checks
before opening state. It also resolves the zcashd binary when supervision is
enabled.

## Open the state

Zebra selects the checkpoint boundary and opens or creates the state database,
restoring eligible non-finalized block backups. See [upgrading](upgrading.md)
for format migrations.

```text
initializing node state
opening database, this may take a few minutes
loaded Zebra state cache
logging database metrics on startup
```

The `Opened Zebra state cache at` message identifies the actual database
path. The `loaded Zebra state cache` event includes `chain_tip`: `None` for
empty state, or the restored block hash and height. Check these fields after
restoring a snapshot or changing `[state] cache_dir`.

Database opening precedes block downloads; a pause here is not a
peer-discovery failure. Format upgrades and checks run on a separate thread
and overlap syncing.

## Initialize peer connections

Zebra opens `[network] listen_addr` and combines configured seed peers with
cached peer addresses.

```text
initializing network
starting the address book updater
resolved seed peer IP addresses
connecting to initial peer set
starting the peer address crawler
```

The DNS message appears when a seed resolves. The `loaded cached peer IP
addresses` event appears when Zebra reads a usable cache. The listener reports
`Opened Zcash protocol endpoint at` followed by its bound address. Individual
seed lookups and peer connections can fail while other connections succeed.

## Initialize verification and the mempool

The mempool holds verified, unmined transactions.

```text
initializing verifiers
initializing block verifier router
initializing syncer
initializing mempool
fully initializing inbound peer request handler
```

The router's log includes `tip` and `max_checkpoint_height`; see
[first sync](run.md#first-sync) for checkpoint and full verification rules.

A separate startup task checks stored block hashes against the compiled
checkpoints. Its first message is:

```text
starting state checkpoint validation
```

This check does not fully revalidate the stored database. A mismatch aborts
startup or operation with an invalid-chain error. With incomplete state and
checkpoint sync enabled, Zebra reports that remaining checkpoints will be
verified during syncing.

On Mainnet and public Testnet, the mempool activates when Zebra is near the
chain tip, rather than during the bulk initial download. Regtest enables it
from height zero. Transactions admitted to the mempool use transaction
verification, not checkpoint verification.

## Start application interfaces

Zebra starts JSON-RPC when `[rpc] listen_addr` is set, logging
`Opened RPC endpoint at` and its bound address without waiting for sync.

Additional services depend on configuration:

| Service | Startup condition | Message |
| --- | --- | --- |
| Block-template updater | Mining configuration enables template preparation, and JSON-RPC or the internal miner can use it | `spawned block template updater task` |
| Managed zcashd | Both `[zcashd_compat] enabled` and `manage_zcashd` are true | `zcashd-compat mode enabled` |
| Indexer gRPC | `[rpc] indexer_listen_addr` is set | `spawning indexer RPC server` |
| Experimental lightwalletd-compatible gRPC | `[rpc] lightwalletd_listen_addr` is set | `spawning lightwalletd gRPC server` |

See [RPC](rpc.md), [mining](mining.md), and
[zcashd compatibility](zcashd-compat.md) for the interface contracts.
The indexer warning `configure an indexer_listen_addr to start the indexer RPC
server` means that this optional listener is disabled; it is not a sync error.

## Launch background tasks

These markers indicate task startup, not completed work:

| Task | Work | Message |
| --- | --- | --- |
| Block gossip | Advertise new best-tip block hashes to peers | `spawning block gossip task` |
| Block notification | Run `[notify] block_notify_command` when configured | `spawning block notify task` |
| Mempool queue checker | Poll the mempool to collect completed transaction-verification work | `spawning mempool queue checker task` |
| Transaction gossip | Advertise transactions added to the mempool | `spawning mempool transaction gossip task` |
| Old database cleanup | Remove eligible old database directories when `[state] delete_old_database` is true | `spawning delete old databases task` |
| Progress reporting | Estimate sync progress, log it, and update health data | `spawning progress logging task` |
| Health server | Serve health and readiness when `[health] listen_addr` is set | `initializing health endpoints` |
| End-of-support check | Enforce the Mainnet release support window | `spawning end of support checking task` |
| Mempool crawler | Request unmined transactions from peers when enabled near the tip | `spawning mempool crawler task` |
| Block syncer | Download and verify blocks needed to extend local state | `spawning syncer task` |

The block-notification marker appears even without a configured
[command](config.md#notify). The [health](health.md) marker appears even when
disabled; an enabled listener also logs `opened health endpoint at` and its
address. See [end of support](run.md#end-of-support) for the release cutoff.

On Regtest, Zebra commits the genesis block locally if it is absent, then runs
the syncer to recover blocks from peers. The internal miner starts last when
its build feature and configuration enable it, with `spawning Zcash miner`.

Once these tasks have been launched, Zebra logs:

```text
spawned initial Zebra tasks
```

This marks task startup, not readiness. Ordinary sync retries do not require
restarting the process.

## Sync the chain

### Download genesis and discover extensions

On Mainnet or Testnet with empty state, Zebra downloads and verifies the
genesis block. The syncer and progress reporter emit:

```text
starting genesis block download and verify
initial sync is waiting to download the genesis block
```

With existing state, Zebra resumes from its stored chain. It sends block
locators, which identify known chain ancestors, to peers and requests hashes
of later blocks. Responses contain at most 500 block hashes. The syncer
schedules downloads and verification concurrently within `[sync]` limits,
then asks for further extensions:

```text
starting sync, obtaining new tips
extending tips
```

### Verify checkpoint ranges

Successful [checkpoint ranges](run.md#first-sync) produce:

```text
verified checkpoint range
```

This event includes `block_count` and `current_range`. Range verification and
state writes are separate stages; incoming blocks do not immediately imply a
higher committed tip.

### Fully verify recent blocks

Above the checkpoint boundary, Zebra fully verifies blocks and transactions.
The progress reporter logs:

```text
estimated progress to chain tip
```

Its fields include `current_height`, `remaining_sync_blocks`, `sync_percent`,
and `time_since_last_state_block`. The estimate uses block times, target block
spacing, and the local clock. It is not a guaranteed completion percentage or
remaining-time prediction.

### Follow new blocks

Near the chain tip, the bulk syncer and block announcements from peers overlap.
Zebra reports the transition with:

```text
close to finishing initial sync, confirming using syncer and gossiped blocks
finished initial sync to chain tip, using gossiped blocks
```

The syncer remains active to recover missing blocks while the inbound handler
downloads and verifies announced blocks. Monitor tip freshness and peers;
see [troubleshooting](troubleshooting.md) if height stops advancing.
