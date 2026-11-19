# Zebra metrics

Zebra exposes counters, gauges, and timing distributions through a Prometheus
HTTP exporter. Prometheus stores the samples; Grafana queries Prometheus to
render dashboards. Metrics complement [health checks](health.md): a
successful scrape does not establish that the node is ready to serve clients.

## Enable the exporter

Use a build with the [`prometheus` feature](install.md#cargo-features).

If no configuration file exists, create one:

```sh
zebrad generate -o zebrad.toml
```

In `zebrad.toml`, set [`[metrics] endpoint_addr`](config.md#metrics):

```toml
[metrics]
endpoint_addr = "127.0.0.1:9999"
```

Start Zebra with that configuration and retrieve the metrics:

```sh
zebrad -c zebrad.toml start
```

From another terminal:

```sh
curl --fail http://127.0.0.1:9999/metrics
```

The exporter provides no authentication or TLS configuration. Bind it to
loopback when Prometheus runs on the same host. For remote scrapes, bind to a
private interface and restrict access with a firewall or authenticated proxy.
Failure to bind the configured address terminates startup.

For Prometheus running on the same host, use:

```yaml
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: zebra
    metrics_path: /metrics
    static_configs:
      - targets: ["127.0.0.1:9999"]
```

For container deployments, use an address reachable from Prometheus; the
supplied Compose stack uses `zebra:9999`.

Configure Grafana's Prometheus data source with the Prometheus server URL,
not Zebra's exporter URL. For a same-host deployment without containers, the
usual Prometheus URL is `http://127.0.0.1:9090`. The Compose stack provisions
its own data source automatically.

## Metric names and types

Zebra's source uses dot-separated names, such as
`sync.estimated_distance_to_tip`. The exporter replaces dots with underscores,
so Prometheus queries use `sync_estimated_distance_to_tip`. Counter names are
not automatically given a `_total` suffix: use the exported name as written.

Counters accumulate during a process run; use `rate()` or `increase()` for
activity over time. Gauges represent a current or most recently measured
value. A metric can be absent until its recording path executes; absence is
not equivalent to zero.

Zebra exports timing distributions as Prometheus **summaries**, with a
`quantile` label and `_sum` and `_count` series, not `_bucket` series:

```promql
rpc_request_duration_seconds{quantile="0.99"}
```

Do not apply `histogram_quantile()` to these Zebra summaries or average their
quantiles across nodes. For a mean duration over five minutes, divide the rate
of `_sum` by the rate of `_count`. Metrics generated separately by Jaeger's
span-metrics connector have their own types and naming.

## Principal operator metrics

The table uses source names; replace dots with underscores in Prometheus.
Suggested alert conditions are operational choices, not Zebra defaults.
Choose sustained thresholds that distinguish initial sync from steady-state
operation and accommodate the network's block production intervals.

| Metric | Exported type | Meaning and alert use |
| --- | --- | --- |
| `zcash.chain.verified.block.height` | Gauge | Best verified chain height, updated as blocks commit. Alert on sustained lack of change when blocks are expected. |
| `sync.estimated_distance_to_tip` | Gauge | Estimated blocks behind the network tip, clamped to zero. Alert on sustained lag after initial sync; this is an estimate, not a peer-agreed height. |
| `sync.estimated_network_tip_height` | Gauge | Estimated network tip height used to interpret the lag gauge. |
| `zcash.net.peers` | Gauge, `network` label | Peer-set size, including ready and busy peers. Alert on zero peers or sustained low connectivity. |
| `candidate_set.recently_live` | Gauge, `network` label | Address-book peers that have responded recently. Use with the peer-set size to diagnose connectivity. |
| `zcash.net.connection.outcomes.total` | Counter | TCP connection and handshake outcomes by `network`, `direction`, `address_family`, `stage`, and `outcome`. Alert on sustained increases in failed outcomes relative to successes. |
| `zebra.net.in.errors` | Counter, `error` label | Incoming message decoding errors. Investigate sustained increases by error category. |
| `sync.verified.block.count` | Counter | Successfully verified blocks in the sync download pipeline. Use its rate to track sync throughput. |
| `state.checkpoint.error.block.count` | Counter | Failed checkpoint-block commits. Investigate increases with the node logs. |
| `rpc.requests.total` | Counter, `method` and `status` labels | Completed RPC requests, separated into `success` and `error`. |
| `rpc.errors.total` | Counter, `method` and `error_code` labels | RPC errors; compare their rate with the request rate for the same method. |
| `rpc.request.duration_seconds` | Summary, `method` label | RPC latency in seconds. Alert on sustained latency for methods whose response-time budget is known. |
| `rpc.active_requests` | Gauge | RPC calls currently being processed. Track sustained concurrency alongside latency. |
| `mempool.failed.verify.tasks.total` | Counter, `reason` label | Mempool download or verification task failures. Distinguish invalid transactions from state or download failures. |
| `zcash.mempool.cost.bytes` | Gauge | Aggregate mempool eviction cost, in cost bytes rather than process memory bytes. Compare with `[mempool] tx_cost_limit`. |
| `zebra.state.rocksdb.total_disk_size_bytes` | Gauge | Sum of RocksDB's `total-sst-files-size` properties across column families, in bytes. Track growth, but monitor filesystem free space separately. |
| `zebra.state.rocksdb.batch_commit.duration_seconds` | Summary | RocksDB batch-write latency in seconds. Use sustained increases to diagnose storage bottlenecks. |

Prometheus also generates `up{job="zebra"}` for scrape success. Alert on zero
or missing scrape data separately from chain progress. This metric comes
from Prometheus, not Zebra.

## Metric catalog

[`docker/observability/metrics-catalog.json`](https://github.com/ZcashFoundation/zebra/blob/main/docker/observability/metrics-catalog.json)
lists every metric Zebra records, with its kind, unit, description, label keys, and
the series names the Prometheus endpoint exports it as. The endpoint also shows each
description as a `# HELP` line. Prometheus names replace the dots in a metric's name
with underscores, and don't add a unit suffix.

Most histograms are exported as Prometheus summaries, with `quantile` series. These
histograms are exported as Prometheus histograms, with `_bucket` series that can be
aggregated across nodes:

- `rpc.request.duration_seconds`
- `zcash.net.peer.handshake.duration_seconds`
- every histogram in [Performance timing](#performance-timing)

The catalog file in a release lists that release's metrics, so you can check your own
dashboards and alerts against the version you run.

## Process metrics

When the Prometheus endpoint is open, or [Sentry](tracing.md#sentry-production-monitoring)
is active, Zebra samples its own process every 15 seconds and records the
standard Prometheus process metrics. They use the usual names, without a prefix,
so existing dashboards and alerts work unchanged. Zebra doesn't record them on
Windows.

| Metric | Unit | Description |
|---|---|---|
| `process_resident_memory_bytes` | bytes | Resident memory size. |
| `process_virtual_memory_bytes` | bytes | Virtual memory size. |
| `process_virtual_memory_max_bytes` | bytes | Maximum virtual memory available. |
| `process_open_fds` | count | Open file descriptors. |
| `process_max_fds` | count | Maximum open file descriptors. |
| `process_threads` | count | OS threads in the process. |
| `process_cpu_seconds_total` | seconds | Total user and system CPU time. |
| `process_start_time_seconds` | seconds | Start time of the process since the Unix epoch. |

Sentry receives all of them. It receives `process_cpu_seconds_total` as the CPU time
used in each minute.

`process_cpu_seconds_total` counts whole seconds, so use a window of a few
minutes, such as `rate(process_cpu_seconds_total[5m])`, to see steady CPU usage
on a lightly loaded node. The two maximums are left out when the limit is
unlimited.

## Block timing

These histograms show how quickly a node accepts new blocks. Official release builds
also send them to [Sentry](tracing.md#sentry-production-monitoring) as distributions
when `SENTRY_DSN` is set, even with the Prometheus endpoint off.

| Metric | Unit | Labels | Description |
|---|---|---|---|
| `zcash.chain.tip.block_delay_seconds` | seconds | `chain.network_upgrade` | Time from a block's header time until it becomes this node's best chain tip. Recorded only for blocks mined at most two minutes before their parent became this node's tip, so most of the blocks a node catches up on after a restart or a stall aren't counted, but a block that arrives while its parent is still being verified is. The block just before a network upgrade activates isn't counted either. Negative when the miner's clock is ahead of this node's. |

`chain.network_upgrade` is the network upgrade at the block's height, such as `Nu6`.
The Prometheus endpoint exports it as the `chain_network_upgrade` label.
Sentry also receives each value with the block's height as the number
`chain.height`. The Prometheus endpoint doesn't export the height, because each
height would become a new series.

The header time is set by the block's miner, so a single value is only approximate.
Nodes that follow the same chain see the same header times, so comparing these
values across nodes over the same period shows which nodes accept blocks sooner.
In Sentry, `chain.height` lets you compare nodes block by block.

## Performance timing

These histograms and counters show where a node spends its time on each block, block
template, and mempool update. Compare them between builds that run on the same
network, hardware, and height range. `zebrad_build_info` has a `commit` label, so
queries can group nodes by the commit they run.

| Metric | Unit | Labels | Description |
|---|---|---|---|
| `zebra.consensus.block.phase.duration_seconds` | seconds | `phase` | Time each phase of full block verification took: `block_checks`, `transaction_verification`, or `state_commit`. Recorded for blocks from sync, gossip, and `submitblock`, but not for block proposals. `block_checks` and `transaction_verification` are recorded when they pass, even if a later phase rejects the block. `state_commit` is recorded only for committed blocks, and includes any wait for the parent block. |
| `state.block_writer.phase.duration_seconds` | seconds | `phase` | Time each phase of committing a block took. Checkpoint-verified blocks record phases that start with `checkpoint_`, from loading the history tree to the RocksDB write. [Block verifier and writer metrics](#block-verifier-and-writer-metrics) describes the phases of other blocks. |
| `zebra.net.block.decode.wait_seconds` | seconds | | Time a received block message waited for a rayon thread to start decoding it. |
| `zebra.net.block.decode.duration_seconds` | seconds | | Time taken to decode a received block message, including that wait. |
| `gossip.block.download.duration_seconds` | seconds | | Time taken to download a gossiped block from a peer. |
| `gossip.block.verify_commit.duration_seconds` | seconds | | Time from sending a downloaded gossiped block to the verifier until it was committed. |
| `rpc.getblocktemplate.build.duration_seconds` | seconds | | Time taken to select transactions and build a block template. |
| `rpc.getblocktemplate.first_template.duration_seconds` | seconds | | Time from a new best chain tip until the first block template on it was served. Miners keep working on the old tip for this long. |
| `rpc.submitblock.outcomes.total` | count | `outcome` | Blocks submitted with `submitblock`, by result: `accepted`, `duplicate`, `duplicate-inconclusive`, `inconclusive`, or `rejected`. |
| `sync.mined.block.announcement.duration_seconds` | seconds | | Time from a mined block's acceptance until its announcement to peers finished. |
| `sync.mined.block.announcement.outcomes.total` | count | `outcome` | Announcements of mined blocks, `completed` or `failed`. |
| `mempool.tip_cleanup.duration_seconds` | seconds | | Time taken to remove a new tip block's transactions and their conflicts from the mempool. |
| `mempool.expiry.duration_seconds` | seconds | | Time taken by each check of the mempool for expired transactions. |

For example, this query shows the 95th percentile of each block verification phase
for each commit:

```promql
histogram_quantile(0.95,
  sum by (le, phase, commit) (
    rate(zebra_consensus_block_phase_duration_seconds_bucket[1h])
    * on (instance) group_left (commit) zebrad_build_info
  )
)
```

## Sync stall metrics

These metrics show why the syncer stopped advancing the chain tip. Their labels have a fixed
set of values. Block heights and hashes are in the matching log fields instead.

Official release builds also send them to [Sentry](tracing.md#sentry-production-monitoring)
when `SENTRY_DSN` is set, even with the Prometheus endpoint off. Sentry receives each counter's
increase once a minute for each set of labels, and each gauge's latest value in the minute it
changes, and at least every 5 minutes.

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `sync.restart.count` | counter | `reason`, `made_progress` | Sync rounds that ended, by why they ended and whether the state tip advanced during the round |
| `sync.block.error.count` | counter | `kind` | Block download and verify errors, including the ones the syncer recovers from without restarting |
| `sync.downloads.oldest_age_seconds` | gauge | none | How long the oldest block still being downloaded or verified has been waiting, or 0 when none are |

`reason` is one of:

- `exhausted_tips`: the syncer downloaded every block its peers advertised. This is the normal end of a round.
- `obtain_tips_timeout` or `obtain_tips_error`: asking peers for new block hashes timed out or failed. If a block they returned was still waiting for network capacity when the timeout fired, the round ends as `block_error` with the `network_ready_timeout` kind instead.
- `extend_tips_error`: asking peers to extend the known hashes failed.
- `sync_step_timeout`: the state tip did not advance for the block verify timeout (8 minutes) during a round. Zebra backs off, then asks for new tips without cancelling pending downloads. A block that was waiting for network capacity is counted in `sync.block.error.count` with the `network_ready_timeout` kind.
- `block_error`: a block download or verify error restarted the sync. The error's `kind` is in the `waiting to restart sync` log.

`kind` names the error, for example `not_found`, `download_timed_out`, `invalid`, `verify_timeout`,
`utxo_lookup_timeout`, `already_verified`, or `cancelled`.

When a round fails without advancing the state tip, Zebra logs a `sync failed without advancing the
state tip` warning with the `reason`, `kind`, and `error`. A `sync_step_timeout` round is not a
failure, so it logs `waiting to restart sync` at INFO instead. The `chain updates have stalled`
warning includes the latest `last_restart` reason, `restarts_without_progress`, `in_flight`, and
`oldest_in_flight_secs`.

## Block verifier and writer metrics

The block verifier times the phases of verifying a block above the checkpoints, and the state's
block writer times the phases of committing a semantically verified block to the non-finalized
state.

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `zebra.consensus.block.phase.duration_seconds` | Histogram | `phase` | Seconds the block verifier spent in one phase of verifying a block, by `phase`. |
| `state.block_writer.phase.duration_seconds` | Histogram | `phase` | Seconds spent in one phase or sub-phase of a block commit, by `phase`. |

Official release builds also send both histograms to
[Sentry](tracing.md#sentry-production-monitoring) as distributions when `SENTRY_DSN` is set, but
only while the node is close to the chain tip.

The block verifier records these phases for blocks submitted for commit, but not for block
proposals:

| `phase` | Measures |
| --- | --- |
| `block_checks` | The block's own checks, from the start of verification until transaction verification starts. Recorded only for blocks that pass them. |
| `transaction_verification` | Verifying the block's transactions. Recorded only when they all verify. |
| `state_commit` | The state's commit of the verified block, whether or not it succeeds. |

A block reused from an already validated proposal skips transaction verification, so it records
only `block_checks` and `state_commit`.

The block writer records these phases, in order:

| `phase` | Measures |
| --- | --- |
| `queue` | The wait between the state service sending a block to the writer and the writer receiving it. |
| `contextual_commit` | Contextual validation and the commit to the non-finalized state, including the sub-phases below. |
| `update_latest_chain_channels` | Publishing the new best chain tip and non-finalized state to readers. |
| `finalization` | Writing blocks past the reorg limit to the finalized state and removing them from the non-finalized state. |

`queue` is recorded for every block the writer receives, and `contextual_commit` for every block
whose parent wasn't rejected, so both include rejected blocks. `update_latest_chain_channels` and
`finalization` are recorded only for committed blocks.

The writer validates a block submitted with `submitblock` while the block verifier is still
verifying it, and keeps the result for the block's commit. The `contextual_commit` phase of such a
block includes that earlier validation, so it can be longer than the block verifier's
`state_commit` phase.

The `contextual_commit` phase has these sub-phases:

| `phase` | Measures |
| --- | --- |
| `transparent_spend` | The transparent spend checks, including UTXO reads from the database. |
| `anchor_reads` | The Sapling and Orchard anchor checks and the Sprout treestate reads. |
| `pickup` | The wait for a rayon thread to start the job that copies the parent chain. |
| `chain_clone` | Copying the parent chain before adding the block to it. |
| `trees` | The note commitment tree appends and root calculations. |
| `chain_updates` | The chain's other indexes and value pools. |
| `history` | The history tree update, which needs the new tree roots. |

The sub-phases are recorded once a block is committed, so rejected blocks have none. They are not
recorded for block proposals, or for mined blocks committed from an already validated proposal. At
startup they include blocks restored from the non-finalized state backup. The `trees` sub-phase runs
in parallel with the sub-phases before `history`, so the sub-phases don't add up to the whole
commit. A block without note commitments has no tree updates, so its sub-phases run in series on
the writer's thread, and its `pickup` is close to zero. A `zebra-rpc` trusted chain sync, which
follows another node's chain without a block writer, also records the sub-phases.

## Memory metrics

These metrics show how much memory `zebrad` uses, and how large the non-finalized state is.

Official release builds also send them to [Sentry](tracing.md#sentry-production-monitoring)
when `SENTRY_DSN` is set, even with the Prometheus endpoint off. Sentry receives the counter as
one total per minute, and each gauge's latest value once a minute.

| Metric | Type | Meaning |
| --- | --- | --- |
| `process.memory.rss_bytes` | gauge | Memory `zebrad` is using (its resident set size). Only on Linux. |
| `process.memory.rss_fraction` | gauge | `process.memory.rss_bytes` as a fraction of the memory `zebrad` can use: the tightest memory limit on its cgroup or any parent cgroup, or the machine's memory if that is smaller. The kernel stops the process near 1. |
| `state.memory.chain.count` | gauge | Non-finalized chains, the best chain and its forks |
| `state.memory.best.chain.length` | gauge | Blocks in the best non-finalized chain. Above the 1,000-block reorg limit, older blocks are waiting to be written to the database. |
| `state.memory.index.entries` | gauge | Blocks, transactions, transparent outputs and spends, and nullifiers indexed across all non-finalized chains, counting the blocks that chains share once |
| `state.memory.best.chain.index.entries` | gauge | The same count for the best chain only |
| `state.memory.fork.evicted.count` | counter | Forks dropped because the node tracked too many |
| `state.memory.backup.restored.block.count` | gauge | Blocks restored from the non-finalized backup at startup |
| `state.memory.backup.restore.duration_seconds` | gauge | How long restoring the non-finalized backup took at startup |

## Network metric aggregation

Network metrics omit per-peer labels to bound the number of time series.
Update dashboards, recording rules, and joins that depend on peer identities:

- Connection-state, request, response, message, and byte metrics have no
  `addr` label. The `zebra.net.connection.state` gauge counts live connections
  by `command`; transitions and disconnects decrement the old state. Request,
  response, and message counters retain `command` labels.
- The counters `zcash.net.peers.connected`, `zcash.net.peers.obsolete`, and
  `zcash.net.peers.missing_services` have no `remote_ip` or `user_agent` labels.
  They count handshake activity, not currently connected peers.
- The gauges `zcash.net.peers.version.connected` and
  `zcash.net.peers.version.obsolete` contain the remote protocol version from
  the most recent corresponding handshake, without `remote_ip` labels.
  They are not peer counts or a distribution of connected versions.
- The `zebra.net.in.errors` counter has no `addr` label.

The counter `zcash.net.peer.version.outcomes.total` provides a software
breakdown through the bounded `implementation` values `zebra`, `zakura`,
`legacy_zcashd`, and `other`. Select `outcome="success"` for successful
handshakes. These remain cumulative handshake counts, not current peer counts.

Other bounded labels include:

- `mempool.failed.verify.tasks.total` `reason`: `in_state`, `state_error`,
  `download_failed`, `cancelled`, `invalid`, `non_standard`, or `timeout`.
- `zebra.net.in.errors` `error`: `io`, `parse`, `utf8_error`,
  `try_from_slice_error`, `try_from_int_error`, `from_hex_error`, `amount`,
  `bad_transaction_balance`, `script`, `opcode`, or `num`.
- Handshake `remote_version` and `negotiated_version`: known protocol versions or
  `other`.
- `zcash.net.peers.missing_services` `missing_service`: the required service the
  rejected peer doesn't provide, `NODE_NETWORK`. The peer doesn't serve full history:
  it doesn't advertise `NODE_NETWORK`, or advertises it with `NODE_PRUNED`. It replaces the
  `remote_services` label, whose value was the service bits the peer sent.
- RPC `method`: registered RPC method names, or `unknown` for unregistered names.

`zcash.net.peers.initial` and `zcash.net.peers.cache` no longer have a `cache`
label with the peer cache file path. The `zcash.net.peers.initial` series without
a `seed` label counts the addresses from the peer cache.

The `seed` label of `zcash.net.peers.initial` names an entry from
`network.initial_mainnet_peers` or `network.initial_testnet_peers` only when it
is one of the default DNS seeders that Zebra ships, such as
`dnsseed.z.cash:8233`. Every other entry, including a custom host name or an IP
address and port, has the value `custom`, so private peer addresses don't reach
metrics in Prometheus or [Sentry](tracing.md#sentry-production-monitoring).

## Mining metrics

Prometheus replaces dots in metric names with underscores. The RPC dashboard includes mining
outcomes and mean build and announcement durations.
Zebra exports these durations as Prometheus histograms, with `_bucket`, `_sum`, and `_count` series.

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `rpc.submitblock.outcomes.total` | Counter | `outcome` | Submission results: `accepted`, `duplicate`, `duplicate-inconclusive`, `inconclusive`, or `rejected`. |
| `rpc.getblocktemplate.build.duration_seconds` | Histogram | None | Completed template construction: transaction selection, coinbase construction, and block roots, in seconds. |
| `rpc.getblocktemplate.first_template.duration_seconds` | Histogram | None | Seconds from best-tip publication to the first template served with that tip as its parent. |
| `sync.mined.block.announcement.outcomes.total` | Counter | `outcome` | Mined-block announcement attempts: `completed` or `failed`. |
| `sync.mined.block.announcement.duration_seconds` | Histogram | None | Seconds from gossip receiving an accepted mined block to successful completion of its announcement's local writes. |

Submission labels follow the zcashd-compatible result, with `accepted` representing JSON `null`.
Operational JSON-RPC errors remain in `rpc.errors.total`; they are not consensus rejections.
Some result variants are reserved by the response type and are not currently returned.

Build timing covers the shared constructor used by background and fallback builders, not state or
mempool reads, executor scheduling, long-poll waits, or cache hits. It records each completed build,
including work superseded before a miner requests it.

First-template latency uses the state's monotonic publication time, not the block header timestamp or
the time the RPC notices a change. It records once per tip transition across RPC clones; repeated
requests do not add samples. Tips with no served template and templates on uncommitted candidates are
excluded. Returning to an earlier tip after a reorg starts a new interval. At startup, timing begins
when the state publishes its loaded tip. The measurement ends when the RPC returns the template,
before network delivery or pool job distribution; it also includes time until a miner requests work.

Announcement completion means every selected connection completed its local write, not that peers
received, validated, or relayed the block. Failed attempts include write errors and timeouts.
Successful durations include peer-readiness waits. While waiting for ready peers, a bounded number of
mined blocks are kept, and the oldest is replaced without a sample when that queue is full. A failed
announcement is retried once; each attempt records its outcome, and a successful retry's duration
starts when gossip first received the block. Early adverts before commit and ordinary tip adverts
are excluded.
No peer count is exported because the broadcast response does not carry one.

Watch increases in rejected submissions and failed announcements alongside rising announcement
durations. A successful local write is not proof against stale blocks, and a stalled attempt has no
completion sample yet. These metrics do not measure network-wide propagation or stale-block rates.

## Observability stack

The [Compose stack][compose-source] builds Zebra and starts Prometheus,
Grafana, Jaeger, and Alertmanager using configuration in
[`docker/observability/`][stack-source]. It persists Zebra's state, Prometheus,
Grafana, and Alertmanager in volumes.

The supplied stack is a development deployment, not a hardened public
service. Zebra's RPC and metrics ports stay on the Compose network, and RPC
requires cookie authentication. The remaining port mappings bind on all host
interfaces; restrict published management ports to loopback or a private
network before starting it. Prometheus's admin and lifecycle APIs are
disabled.

Export `GF_SECURITY_ADMIN_PASSWORD` with a strong, unique password; Compose
rejects an unset or empty value. Run from the repository root:

```sh
docker compose -f docker/docker-compose.observability.yml up -d
docker compose -f docker/docker-compose.observability.yml logs -f zebra
```

| Service | Host URL |
| --- | --- |
| Grafana | <http://localhost:3000> |
| Prometheus | <http://localhost:9094> |
| Jaeger | <http://localhost:16686> |
| Alertmanager | <http://localhost:9093> |

Log in to Grafana as `admin` with the password supplied through
`GF_SECURITY_ADMIN_PASSWORD`. It initializes new Grafana volumes only; change
existing account passwords in Grafana. The stack provisions the Prometheus
and Jaeger data sources and the dashboards in
`docker/observability/grafana/dashboards/`. The home dashboard is
`zebra_overview.json`; other dashboards include `peers.json`,
`network_health.json`, `syncer.json`, `mempool.json`, `rocksdb.json`,
`rpc_metrics.json`, `rpc_tracing.json`, and `value_pools.json`. For a separate
Grafana deployment, import the JSON files and select its Prometheus data
source.

The file `docker/observability/prometheus/prometheus.yaml` scrapes
`zebra:9999` and Jaeger's span metrics at `jaeger:8889`, every 15 seconds.
Zebra exports traces to `http://jaeger:4318` with 100% sampling in this stack.
Reduce `[tracing] opentelemetry_sample_percent` through the Compose environment
when lower trace volume is required; see [OpenTelemetry export](tracing.md#opentelemetry-export).

Review `docker/observability/prometheus/rules/zebra_alerts.yml` before relying
on its alerts. Its latency rules use exported summary quantiles: RPC p99 above
two seconds and successful-handshake p95 above two seconds, each sustained for
five minutes. Zebra abandons handshakes after three seconds, so slower ones
never reach the handshake series. The RPC latency panels show per-method
quantiles, not a combined percentile across methods or nodes.
The receivers in `docker/observability/alertmanager/alertmanager.yml` have no
notification destinations; configure them before expecting notifications.

Stop the stack without deleting its volumes:

```sh
docker compose -f docker/docker-compose.observability.yml down
```

[compose-source]: https://github.com/ZcashFoundation/zebra/blob/main/docker/docker-compose.observability.yml
[stack-source]: https://github.com/ZcashFoundation/zebra/tree/main/docker/observability
