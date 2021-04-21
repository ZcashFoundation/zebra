# Tracing Zebra

Zebra supports dynamic tracing, configured using the config's 
[`TracingSection`][tracing_section] and (optionally) an HTTP RPC endpoint.

If the `endpoint_addr` is specified, `zebrad` will open an HTTP endpoint
allowing dynamic runtime configuration of the tracing filter. For instance,
if the config had `endpoint_addr = '127.0.0.1:3000'`, then

* `curl -X GET localhost:3000/filter` retrieves the current filter string;
* `curl -X POST localhost:3000/filter -d "zebrad=trace"` sets the current filter string.

See the [`filter`][filter] documentation for more details.

Zebra also has support for:

* Generating [flamegraphs] of tracing spans, configured using the
[`flamegraph`][flamegraph] option.
* Sending tracing spans and events to [systemd-journald][systemd_journald],
on Linux distributions that use `systemd`. Configured using the
[`use_journald`][use_journald] option.

[tracing_section]: https://doc.zebra.zfnd.org/zebrad/config/struct.TracingSection.html
[filter]: https://doc.zebra.zfnd.org/zebrad/config/struct.TracingSection.html#structfield.filter
[flamegraph]: https://doc.zebra.zfnd.org/zebrad/config/struct.TracingSection.html#structfield.flamegraph
[flamegraphs]: http://www.brendangregg.com/flamegraphs.html
[systemd_journald]: https://www.freedesktop.org/software/systemd/man/systemd-journald.service.html
[use_journald]: https://doc.zebra.zfnd.org/zebrad/config/struct.TracingSection.html#structfield.use_journald
