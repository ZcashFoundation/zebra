# Tracing Zebra

Zebra records structured events and spans through Rust's `tracing` library.
Events are individual log records; spans attach context to an operation and
its nested work. Zebra writes formatted logs to standard output unless a
log file is configured. Optional exporters send this instrumentation to
journald, Sentry, or an OpenTelemetry collector.

## Configuration

To write plain-text logs to a file:

```toml
[tracing]
filter = "info,zebra_network=warn"
log_file = "/var/log/zebra/zebrad.log"
use_color = false
buffer_limit = 128000
```

Create a writable log directory owned by the Zebra account. If Zebra runs
with elevated permissions, prevent other users from modifying that directory
or its parents. Zebra appends to the file; it does not configure log rotation.
Set `use_color = false` for plain-text files: formatted-log color detection
uses standard output, even when `log_file` redirects the logs.

For progress-bar log paths, buffer limits, and color overrides, see the
[configuration reference](config.md#tracing).

## Filters and compiled log levels

The filter uses [`tracing-subscriber` directives][filter-syntax]. A bare level
sets the global threshold; `target=level` sets a threshold for a Rust target,
usually a module path. Separate directives with commas. Levels are `error`,
`warn`, `info`, `debug`, and `trace`; `off` disables a target.

For example, `warn,zebrad=info,zebra_network=debug` selects warnings globally,
information from `zebrad`, and debug events from `zebra_network`. Directives
can also select spans and their fields; see the syntax reference before using
field filters.

Override the configured filter with any of these command forms:

```sh
zebrad -c zebrad.toml --filters 'info,zebra_network=debug'
zebrad -c zebrad.toml --filters 'info,zebra_network=debug' start
zebrad -c zebrad.toml start 'info,zebra_network=debug'
```

The top-level `--filters` option applies with or without an explicit `start`.
Positional filters after `start` take precedence over `--filters` and are
joined with commas. An explicit filter takes precedence over the default
selected by `-v`.

The utility commands `generate` and `tip-height` use
[fixed filters](config.md#tracing) and disable flamegraph collection.

Default release builds compile out debug and trace instrumentation through
`release_max_level_info`; neither `-v` nor runtime filters can restore it.
Build from the repository with default features disabled to retain all levels:

```sh
cargo install --locked --path zebrad --no-default-features \
  --features 'progress-bar,prometheus,sentry,opentelemetry'
```

For other compile-time ceilings, see [Cargo features](install.md#cargo-features).

## Dynamic tracing

Build with the `filter-reload` feature and configure a loopback listener:

```toml
[tracing]
endpoint_addr = "127.0.0.1:3000"
```

Read or replace the running filter:

```sh
curl --fail http://127.0.0.1:3000/filter
curl --fail --data 'info,zebra_network=debug' http://127.0.0.1:3000/filter
```

The server accepts `GET /filter` and `POST /filter`; the POST body is the new
UTF-8 filter string. A reload changes the running subscriber, not the
configuration file. The endpoint has no authentication. Keep it on loopback
or a restricted management network.

Filter reloading is disabled in builds using `tokio-console` with
`tokio_unstable`, even if the HTTP endpoint is enabled. See
[Tokio console](../dev/tokio-console.md).

## Journald logging

Build with the `journald` Cargo feature and set `[tracing] use_journald = true`
on Linux systems running systemd. Zebra adds a journald subscriber for events
and spans; formatted output to standard output or `log_file` remains enabled.
Failure to initialize the journald subscriber fails tracing initialization.
Setting the field without the Cargo feature logs a warning instead of
activating journald.

## Flamegraphs

A flamegraph visualizes time spent in nested tracing spans, not samples of
every function on the CPU stack. Build with the `flamegraph` Cargo feature
and set a writable output path:

```toml
[tracing]
flamegraph = "/var/log/zebra/spans"
```

Zebra records folded span stacks in `spans.folded` and generates `spans.svg`
during its shutdown hook. An extension in the configured path is replaced by
these extensions. An abrupt process termination can prevent SVG generation.
Use a directory protected against modification by other users, and compile
in the span levels needed for the investigation.

## OpenTelemetry export

[OpenTelemetry](https://opentelemetry.io/docs/concepts/signals/traces/)
collects spans as traces for operation-level analysis. With the
[`opentelemetry` feature](install.md#cargo-features), configure an endpoint:

```toml
[tracing]
opentelemetry_endpoint = "http://127.0.0.1:4318"
opentelemetry_service_name = "zebra"
opentelemetry_sample_percent = 10
```

Zebra exports batches using OTLP, the OpenTelemetry Protocol, over HTTP with
binary Protocol Buffers encoding. It appends `/v1/traces` unless that suffix
is already present. Use an OTLP HTTP receiver, not a gRPC endpoint. The
[observability stack](metrics.md#observability-stack) supplies Jaeger as a
receiver and trace viewer.

Set `[tracing] opentelemetry_sample_percent` to an integer from 0 through 100:
Zebra samples no traces at 0, all at 100, and uses trace-ID ratio sampling
between them. See the [configuration reference](config.md#tracing) for
environment fallbacks and their nonstandard integer-percentage syntax.

Sampling cannot recover instrumentation removed by compile-time level
features or excluded by the tracing filter. Zebra continues without trace
export if exporter initialization fails.

## Sentry production monitoring

[Sentry](https://sentry.io/) receives error reports, diagnostic logs, and
release health sessions. With the [`sentry` feature](install.md#cargo-features),
set `SENTRY_DSN` to a valid project ingestion address to enable reporting.
Leave it unset to disable reporting.

With Sentry enabled, Zebra sends:

- errors and panics as Sentry issues;
- warnings, errors, and the once-a-minute sync progress logs as Sentry Logs,
  including a log each time the best chain tip enters a different network upgrade;
- release health sessions: each `zebrad` run is split into daily sessions, and
  the last one ends as exited on a clean shutdown and as crashed on a panic or a
  fatal error. Every session of a run has the same user ID, so Sentry's
  crash-free users rate is the share of runs without a crash;
- every metric in the [metric catalog](metrics.md#metric-catalog), the same
  metrics the Prometheus endpoint exports, as Sentry metrics.

Zebra sends metrics to Sentry once a minute, and again when it shuts down or
panics. Each time, it sends:

- for each counter, its increase since it was last sent, once for each set of
  labels that increased. A counter that holds a running total, such as
  `process_cpu_seconds_total`, also sends its increase;
- for each gauge, its latest value, once for each set of labels whose value
  changed since it was last sent. A gauge that doesn't change is sent again
  every 5 minutes, so charts with an interval of 5 minutes or more show its
  steady value;
- for each histogram, at most 20 of the values recorded since the previous
  minute for each set of labels, as distributions. When more values were
  recorded, Zebra picks 20 of them at random, each value as likely as any
  other, and keeps only those while it records. Histograms recorded about once
  per block near the chain tip, such as `zcash.chain.tip.block_delay_seconds`,
  send every value.

Each minute sends at most 1,500 metrics. The Sentry SDK sends metrics in
envelopes of 100, and queues at most 30 envelopes for events, logs, release
health sessions, and metrics together. Before it sends a minute's metrics,
Zebra waits up to 2 seconds for the SDK to send everything already in that
queue. If the SDK is still sending, for example because your Sentry server is
slow or unreachable, Zebra sends no metrics that minute. So metrics leave about
half of the queue free, even while the SDK can't send anything. Zebra also
waits a second between envelopes, so the SDK can send each one before the next
is queued.

When Zebra shuts down or panics, it ends the release health session first. Then
it stops waiting between envelopes and, after the same check of the queue,
sends the metrics that are due. The SDK drops any envelope that arrives while
its queue is full, and counts it in the client reports it sends to Sentry.

When more than 1,500 metrics are due in a minute, Zebra sends counters first,
then gauges that changed, then gauges due for their 5-minute refresh, then
histogram values. A counter or gauge that doesn't fit is sent in a later minute,
with its whole increase or its latest value. Histogram values that don't fit are
dropped.

Zebra sends at most 256 sets of labels for each metric to Sentry. Values for any
other set of labels aren't sent to Sentry, but the Prometheus endpoint still
exports them. Zebra's labels have a fixed set of values, so a metric only reaches
this limit if one of its labels keeps taking new values.

The `zebrad.sentry.metrics.dropped.total` counter counts the values Zebra didn't
send to Sentry when they were due, by `reason`:

- `flush_budget`: the minute already had 1,500 metrics;
- `transport_backlog`: the Sentry SDK was still sending what was queued
  before, so the minute sent no metrics. Counters and gauges are sent in a
  later minute, and histogram values are dropped. If this count keeps growing,
  check that Zebra can reach your Sentry server;
- `label_set_cap`: the value was recorded for a set of labels over the limit.

Like every other metric, this counter reaches both Sentry and the Prometheus
endpoint. When Zebra starts leaving values out, it logs a warning with the
counts and the names of any metrics that reached the label limit. While values
keep being left out, it repeats the warning at most once an hour.

If Zebra can't start the thread that sends metrics, it logs an error and sends
no metrics to Sentry, while the Prometheus endpoint keeps working.

Sentry metrics carry each metric's labels as attributes. The `seed` label of
`zcash.net.peers.initial` names only Zebra's default DNS seeders, and has the
value `custom` for any other configured initial peer.
`zcash.chain.tip.block_delay_seconds` also carries the block's height as
`chain.height`.

Events, logs, and metrics carry the `chain.network` tag. Once the progress
task observes a chain tip, events and logs also carry `chain.height` and
`chain.network_upgrade`. Logs carry these fields as attributes; the height
is numeric for aggregation. Metrics carry `chain.network_upgrade`, and
`chain.near_tip`, which is `true` when the syncer was close to the chain tip
as the metrics were sent. Use `chain.near_tip` to leave out the values that
an initial sync records. Metrics
also carry `process.uptime_seconds`, the whole seconds since `zebrad` started,
so queries can leave out the values recorded just after a restart.

| Environment variable | Effect |
| --- | --- |
| `SENTRY_ENVIRONMENT` | Sets the environment attached to reports and sessions. |
| `SENTRY_RELEASE` | Overrides the release name; otherwise Zebra uses the build version printed by `zebrad --version`. |

Errors and panics include stack traces, with Zebra's own frames marked in-app.
In the official Docker images, Zebra's source files appear under `/zebra/`.
Sentry doesn't create code mappings for Rust, so to show suspect commits, connect
Sentry to GitHub and add a code mapping with the stack trace root `/zebra/`, an
empty source code root, the `ZcashFoundation/zebra` repository, and the `main`
branch. The release binaries on GitHub are stripped, so their stack frames have
no function names or source locations.

Review the destination and access controls before enabling reporting: error
context and panic reports leave the node. For deployment metadata, see
[GCP deployment operations](../dev/gcp-deployment-operations.md); for event
selection, see the [Sentry integration][sentry-source].

[filter-syntax]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives
[sentry-source]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/sentry.rs
