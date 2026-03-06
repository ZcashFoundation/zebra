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

## Sentry Production Monitoring

Compile Zebra with `--features sentry` to monitor it using [Sentry][sentry] in production.

[tracing_section]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html
[filter]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.filter
[flamegraph]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.flamegraph
[flamegraphs]: http://www.brendangregg.com/flamegraphs.html
[systemd_journald]: https://www.freedesktop.org/software/systemd/man/systemd-journald.service.html
[use_journald]: https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html#structfield.use_journald
[sentry]: https://sentry.io/welcome/
