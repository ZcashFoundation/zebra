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

| Metric or label | Values |
| --- | --- |
| `mempool.failed.verify.tasks.total`, `reason` | `in_state`, `state_error`, `download_failed`, `cancelled`, `invalid`, `timeout` |
| `zebra.net.in.errors`, `error` | `io`, `parse`, `utf8_error`, `try_from_slice_error`, `try_from_int_error`, `from_hex_error`, `amount`, `bad_transaction_balance`, `script`, `opcode`, `num` |
| Handshake `remote_version` and `negotiated_version` | Protocol versions defined by Zebra, or `other`; arbitrary received numbers do not create new labels. |
| RPC `method` | Registered RPC method names, or `unknown` for unregistered names. |

## Observability stack

The [Compose stack][compose-source] builds Zebra and starts Prometheus,
Grafana, Jaeger, and Alertmanager using configuration in
[`docker/observability/`][stack-source]. It persists Zebra's state, Prometheus,
Grafana, and Alertmanager in volumes.

The supplied stack is a development deployment, not a hardened public
service. Its port mappings bind on all host interfaces, and its Zebra RPC
server disables cookie authentication. Restrict the published management
ports to loopback or a private network before starting it. Configure RPC
access controls before exposing port 8232.

Run from the repository root:

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
| Zebra metrics | <http://localhost:9999/metrics> |

Grafana's initial credentials are `admin` / `admin`; change the password at
first login. The stack provisions the Prometheus and Jaeger data sources and
the dashboards in `docker/observability/grafana/dashboards/`. The home
dashboard is `zebra_overview.json`; other dashboards include `peers.json`,
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
on its alerts. In Zebra 7.0.0, its RPC and handshake latency rules query
`_bucket` series that the default Zebra exporter does not produce. Its generic
error-rate rule also refers to `zebra_errors_total`, which has no Zebra metric
producer. Use the actual series listed above when adapting rules and panels.
The receivers in `docker/observability/alertmanager/alertmanager.yml` have no
notification destinations; configure them before expecting notifications.

Stop the stack without deleting its volumes:

```sh
docker compose -f docker/docker-compose.observability.yml down
```

[compose-source]: https://github.com/ZcashFoundation/zebra/blob/main/docker/docker-compose.observability.yml
[stack-source]: https://github.com/ZcashFoundation/zebra/tree/main/docker/observability
