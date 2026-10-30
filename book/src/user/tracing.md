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

- Error events as issues and Sentry Logs; warnings as Logs and breadcrumbs.
  Breadcrumbs are contextual records attached to later issues.
- Sync progress logs with a 60-second reporting interval, and a log when the
  best chain tip changes network upgrade. Other information-level events become
  breadcrumbs rather than standalone Logs. Runtime filters still apply.
- Panics as fatal, unhandled exceptions, with the full panic report and source
  location as context.
- Release health sessions, rotated every 24 hours. Graceful shutdown ends a
  session as exited; a panic, crash, or forced shutdown ends it as crashed.
  Sessions share a random identifier per run, so crash-free users measures
  runs, not people. A process killed before shutdown handling cannot send its
  final session update.

Events carry `chain.network` and, once the progress task observes a chain tip,
`chain.height` and `chain.network_upgrade`. Logs carry the same information
as attributes; the height is numeric for aggregation.

| Environment variable | Effect |
| --- | --- |
| `SENTRY_ENVIRONMENT` | Sets the environment attached to reports and sessions. |
| `SENTRY_RELEASE` | Overrides the release name; otherwise Zebra uses the build version printed by `zebrad --version`. |

Review the destination and access controls before enabling reporting: error
context and panic reports leave the node. For deployment metadata, see
[GCP deployment operations](../dev/gcp-deployment-operations.md); for event
selection, see the [Sentry integration][sentry-source].

[filter-syntax]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives
[sentry-source]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/sentry.rs
