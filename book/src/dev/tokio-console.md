# Tokio console

The [Tokio console] displays asynchronous tasks, resources, and operations.
Zebra's [subscriber][subscriber initialization] requires the
[`tokio-console` feature](../user/install.md#cargo-features) and experimental
`tokio_unstable` configuration.

## Setup

Build from the repository root with both the feature and the compiler
configuration enabled:

```sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --locked -p zebrad \
  --no-default-features --features tokio-console --bin zebrad
cargo install --locked tokio-console
```

The `--no-default-features` option removes `release_max_level_info`. Do not
enable any `max_level_*` or `release_max_level_*` features: they discard trace
events that runtime filters cannot restore. Zebra already enables Tokio's
`tracing` feature.

Start the instrumented binary with the configuration for the node being
investigated:

```sh
./target/debug/zebrad --config zebrad.toml start
```

In another terminal, connect the console client:

```sh
tokio-console
```

The subscriber and client use the same [default endpoint](../user/run.md#ports).
Keep this diagnostics endpoint on loopback. Override the server address with
`TOKIO_CONSOLE_BIND` and configure the client to match; see the
[subscriber configuration] for other environment variables.

## Interpreting the output

Inspect polling and wake activity for tasks consuming time or remaining
pending. The resource view links tasks to synchronization primitives.
For native call stacks and cryptographic computation, use
[sampling profiles](profiling-and-benchmarking.md#profiling).

If the console connects but shows no task data, check the build flags and
compile-time tracing filters before changing the node's log filter. Zebra uses
`console_subscriber::spawn()`, which installs the console's required target
filter separately from ordinary log output.

[Tokio console]: https://github.com/tokio-rs/console
[subscriber initialization]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/tracing/component.rs
[subscriber configuration]: https://docs.rs/console-subscriber/0.5.0/console_subscriber/fn.spawn.html
