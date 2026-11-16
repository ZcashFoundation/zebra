# Profiling and benchmarking Zebra

Record the commit, Rust toolchain, Cargo features, configuration, network,
height range, and machine. Keep them fixed except for the change measured.

## Profiling

The [samply] sampling profiler records native call stacks. Zebra defines a
`profiling` Cargo profile in [`.cargo/config.toml`][cargo-config]; it inherits
release optimizations and enables debug information for workspace crates.
Release-profile package overrides disable debug information for dependencies.

Install the profiler and build from the repository root:

```sh
cargo install --locked samply
cargo build --locked -p zebrad --profile profiling --bin zebrad
samply record ./target/profiling/zebrad --config zebrad.toml start
```

Stop the workload with `Ctrl+C` after collecting the intended interval.
Samply opens the profile in the Firefox Profiler interface and serves local
symbol information. Profiles remain local unless uploaded through the
profiler. On Linux, samply needs permission to use performance events; follow
its [platform setup instructions][samply] rather than running the node as
root.

Use the [Tokio console](tokio-console.md#setup) for async polling and wake activity.

### Tracing flamegraphs

Zebra's `flamegraph` feature records time in tracing spans, which are named
instrumented operations. It does not sample every native function. Build
without compile-time log-level restrictions when collecting detailed spans:

```sh
cargo build --locked -p zebrad --release --no-default-features \
  --features flamegraph --bin zebrad
```

Add the output path and the required target filters to the node's profiling
configuration:

```toml
[tracing]
flamegraph = "zebra-profile"
filter = "info,zebrad=trace,zebra_consensus=trace,zebra_state=trace"
```

Run that binary with the configuration file:

```sh
./target/release/zebrad --config zebrad-profile.toml start
```

The [flamegraph layer] records folded tracing stacks in
`zebra-profile.folded`. During normal component shutdown, Zebra flushes the
folded data and generates `zebra-profile.svg`. Any extension supplied in the
configuration is replaced with `.folded` and `.svg`. A forced termination can
prevent the SVG from being generated. The output directory must already exist
and be writable.

Trace-level logging and span collection add work to the process. Use the same
instrumentation when comparing profiles, and use uninstrumented benchmark
runs for the performance claim.

## Benchmarking

Zebra has [Criterion] benchmarks in `zebra-chain/benches/` and
`zebra-consensus/benches/`. Run an individual target from the workspace root:

| Crate | Target | Operation | Command |
| --- | --- | --- | --- |
| `zebra-chain` | `block` | Block serialization and deserialization | `cargo bench --locked -p zebra-chain --bench block --features bench` |
| `zebra-chain` | `transaction` | Transaction serialization and deserialization | `cargo bench --locked -p zebra-chain --bench transaction` |
| `zebra-chain` | `redpallas` | Batched and individual RedPallas signature verification | `cargo bench --locked -p zebra-chain --bench redpallas` |
| `zebra-consensus` | `groth16` | Sprout Groth16 proof verification | `cargo bench --locked -p zebra-consensus --bench groth16` |
| `zebra-consensus` | `halo2` | Orchard Halo2 proof verification | `cargo bench --locked -p zebra-consensus --bench halo2` |
| `zebra-consensus` | `sapling` | Sapling proof and signature verification | `cargo bench --locked -p zebra-consensus --bench sapling` |

The transaction target uses version 1 through 5 vectors; the Halo2 target uses
pre-NU6.2 Orchard vectors and its verifying key. Add representative inputs for
paths these benchmarks omit.

To compare two revisions locally, save a Criterion baseline before changing
the source:

```sh
cargo bench --locked -p zebra-chain --bench transaction -- --save-baseline before
```

After switching to the proposed revision, rerun with the same build settings
and target directory:

```sh
cargo bench --locked -p zebra-chain --bench transaction -- --save-baseline after
cargo install --locked critcmp
critcmp before after
```

Keep other CPU- and I/O-intensive jobs off the benchmark host during the
measurement. Repeat comparisons that are close to run-to-run variation;
report the workload and measured uncertainty rather than only a percentage.

### CI comparisons and dashboard

Dispatch the [benchmark workflow] to update the [benchmark dashboard], or
apply the `run-benchmarks` pull request label for a base/head comparison.
See [benchmark CI](continuous-integration.md#benchmarks) for triggers, target
registration, and result handling. Confirm small differences on a controlled
host.

### Local synchronization workload

Measure a second node downloading from a synchronized source node to reduce
public-peer variability. Initial peer lists are seeds, not allowlists; enforce
isolation outside Zebra to restrict connections to the source. Disabling the
peer cache and delaying crawls do not prevent peer requests when the peer set
is busy.

Give the source its own persistent state directory. Replace the example path
with the synchronized Mainnet cache:

```toml
[network]
network = "Mainnet"
listen_addr = "127.0.0.1:8233"

[state]
cache_dir = "/path/to/synced-mainnet-cache"
```

Build the binary and start the source outside the timed interval:

```sh
cargo build --locked --release -p zebrad --bin zebrad
./target/release/zebrad --config zebrad-source.toml start
```

Use a separate configuration for the measured node:

```toml
[network]
network = "Mainnet"
listen_addr = "127.0.0.1:8234"
initial_mainnet_peers = ["127.0.0.1:8233"]
cache_dir = false
peerset_initial_target_size = 1
crawl_new_peer_interval = "10d"

[state]
ephemeral = true
debug_stop_at_height = 10_000
```

The measured node uses a temporary database, flushing and removing it when
finalized state reaches height 10,000. A crash can leave temporary files.
The height-triggered exit bypasses tracing shutdown and SVG generation.
Time the process directly:

```sh
time ./target/release/zebrad --config zebrad-measured.toml start
```

Use distinct ports and state directories. Keep the source running with the
required blocks and wait for the measured process to exit between runs.
Operating-system and source-node caches survive database recreation; record
whether measurements use warm or cold caches.

The 10,000-block workload measures checkpoint synchronization. To measure full
verification, set `[consensus] checkpoint_sync = false` and stop above the
[mandatory checkpoint coverage](zebra-checkpoints.md#checkpoint-coverage-and-spacing).

[samply]: https://github.com/mstange/samply
[cargo-config]: https://github.com/ZcashFoundation/zebra/blob/main/.cargo/config.toml
[flamegraph layer]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/tracing/flame.rs
[Criterion]: https://github.com/bheisler/criterion.rs
[benchmark workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/benchmarks.yml
[benchmark dashboard]: https://zebra.zfnd.org/dev/bench/
