# Tracing Zebra

## Dynamic Tracing

Zebra supports dynamic tracing, configured using the config's
[`TracingSection`][tracing_section] and an HTTP RPC endpoint.

Activate this feature using the `filter-reload` compile-time feature,
and the [`filter`][filter] and `endpoint_addr` runtime config options.

If the `endpoint_addr` is specified, `zebrad` will open an HTTP endpoint
allowing dynamic runtime configuration of the tracing filter. For instance,
if the config had `endpoint_addr = '127.0.0.1:3000'`, then

- `curl -X GET localhost:3000/filter` retrieves the current filter string;
- `curl -X POST localhost:3000/filter -d "zebrad=trace"` sets the current filter string.

See the [`filter`][filter] documentation for more details.

## `journald` Logging

Zebra can send tracing spans and events to [systemd-journald][systemd_journald],
on Linux distributions that use `systemd`.

Activate `journald` logging using the `journald` compile-time feature,
and the [`use_journald`][use_journald] runtime config option.

## Flamegraphs

Zebra can generate [flamegraphs] of tracing spans.

Activate flamegraphs using the `flamegraph` compile-time feature,
and the [`flamegraph`][flamegraph] runtime config option.

## OpenTelemetry Export

Official Zebra release builds include OpenTelemetry support. Export is disabled
until you configure an OpenTelemetry endpoint using the tracing config or the
`OTEL_EXPORTER_OTLP_ENDPOINT` environment variable.

## Sentry Production Monitoring

Official Zebra release builds include Sentry support. Sentry is only activated
when the `SENTRY_DSN` environment variable is set.

When Sentry is active, Zebra sends:

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
value `custom` for any other configured initial peer. Histograms also carry their
event attributes, without the `event.` prefix, such as `chain.height` on
`zcash.chain.tip.block_delay_seconds`.

Events, logs, and metrics carry the `chain.network` tag. Once the node has a
chain tip, events and logs also carry `chain.height` and `chain.network_upgrade`.
Metrics carry `chain.network_upgrade`, and `chain.near_tip`, which is `true`
when the syncer was close to the chain tip as the metrics were sent. Use
`chain.near_tip` to leave out the values that an initial sync records. Metrics
also carry `process.uptime_seconds`, the whole seconds since `zebrad` started,
so queries can leave out the values recorded just after a restart.

You can optionally set `SENTRY_ENVIRONMENT` to control the environment name
attached to Sentry events. Zebra also tags events with the git SHA when
available, preferring the runtime `GITHUB_SHA` (full commit SHA on GitHub
Actions) and falling back to the build-baked `SHORT_SHA` or `VERGEN_GIT_SHA`.
When it runs under GitHub Actions it reads standard `GITHUB_*` metadata plus
the optional `CI_TEST_ID` runtime variable for CI context. If `github-slug-action` exports `GITHUB_REF_POINT_SLUG_URL`, Zebra
uses that slugged branch or tag name for the `git.ref` tag, and CI workflows
can pass `CI_PR_NUMBER` and `CI_TEST_ID` for additional correlation. These
values are read at runtime, so container images do not need CI-specific build
arguments, and the `ZEBRA_*` environment namespace remains reserved for Zebra
configuration.

Zebra names its Sentry release after its build version, which `zebrad --version`
prints, for example `7.0.0-rc.0+git.6d1e414`. Set `SENTRY_RELEASE` to use a
different name.

Errors and panics include stack traces, with Zebra's own frames marked in-app.
In the official Docker images, Zebra's source files appear under `/zebra/`.
Sentry doesn't create code mappings for Rust, so to show suspect commits, connect
Sentry to GitHub and add a code mapping with the stack trace root `/zebra/`, an
empty source code root, the `ZcashFoundation/zebra` repository, and the `main`
branch. The release binaries on GitHub are stripped, so their stack frames have
no function names or source locations.

[tracing_section]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html
[filter]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.filter
[flamegraph]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.flamegraph
[flamegraphs]: http://www.brendangregg.com/flamegraphs.html
[systemd_journald]: https://www.freedesktop.org/software/systemd/man/systemd-journald.service.html
[use_journald]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.use_journald
[sentry]: https://sentry.io/welcome/
