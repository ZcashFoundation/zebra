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

- for each counter, its increase since the previous minute, once for each set
  of labels that increased. A counter that holds a running total, such as
  `process_cpu_seconds_total`, also sends its increase;
- for each gauge, its latest value, once for each set of labels;
- for each histogram, at most 20 of the values recorded since the previous
  minute for each set of labels, as distributions. When more values were
  recorded, Zebra picks 20 spread across the minute. Histograms recorded about
  once per block near the chain tip, such as
  `zcash.chain.tip.block_delay_seconds`, send every value.

These limits keep one minute of metrics within the Sentry SDK's queue, even
during an initial sync. If Zebra can't start the thread that sends them, it logs
an error and sends no metrics to Sentry, while the Prometheus endpoint keeps
working.

Sentry metrics carry each metric's labels as attributes, including the DNS
seeder host names from your network configuration in the `seed` label of
`zcash.net.peers.initial`. Histograms also carry their event attributes, without
the `event.` prefix, such as `chain.height` on
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
