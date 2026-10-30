# Building and installing Zebra

Install a release archive on supported Linux systems, use the
[Docker image](docker.md), or build from source. Check
[system requirements](requirements.md) and [platform support](supported-platforms.md)
before provisioning the node.

## Pre-built binaries

The [GitHub release](https://github.com/ZcashFoundation/zebra/releases/tag/v7.0.0)
provides these archives:

| Architecture | Archive |
| --- | --- |
| 64-bit x86 | `zebrad-7.0.0-x86_64-unknown-linux-gnu.tar.gz` |
| 64-bit ARM | `zebrad-7.0.0-aarch64-unknown-linux-gnu.tar.gz` |

Both require glibc 2.34 or newer and the C++ runtime library, `libstdc++`.
Documented runtime baselines include Ubuntu 22.04, Debian 12, RHEL 9, and
Amazon Linux 2023, or newer releases. RocksDB and the Zcash C++ script library
are linked statically. These are GNU/Linux binaries, not macOS, Windows, or
musl binaries.

Each archive contains `zebrad`, both license files, and a README. The release
also supplies a checksum sidecar for each archive, a combined `SHA256SUMS`,
and its Cosign signature bundle, `SHA256SUMS.sigstore.json`. GitHub stores a
build-provenance attestation for each archive.

### Install with cargo-binstall

After installing [cargo-binstall](https://github.com/cargo-bins/cargo-binstall),
install the latest published Zebra release:

```sh
cargo binstall zebrad
```

To select this release explicitly:

```sh
cargo binstall --version 7.0.0 zebrad
```

The [package metadata](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/Cargo.toml#L36-L40)
does not configure verification of Zebra's signed checksum manifest. Use the
manual procedure below when that verification is required.

### Download and verify an archive

Install [GitHub CLI](https://cli.github.com/) and
[Cosign](https://docs.sigstore.dev/cosign/system_config/installation/).
The following Linux procedure also uses GNU `sha256sum`, `tar`, and `install`.
Select `aarch64-unknown-linux-gnu` instead of `x86_64-unknown-linux-gnu` on ARM64.
Run the commands in a new directory so unrelated files do not affect checksum
verification.

```sh
set -eu
version=7.0.0
target=x86_64-unknown-linux-gnu
archive="zebrad-${version}-${target}.tar.gz"
mkdir "zebrad-${version}-${target}"
cd "zebrad-${version}-${target}"

gh release download "v${version}" --repo ZcashFoundation/zebra \
  --pattern "${archive}" \
  --pattern SHA256SUMS \
  --pattern SHA256SUMS.sigstore.json

gh attestation verify "${archive}" \
  --repo ZcashFoundation/zebra \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml \
  --source-ref "refs/tags/v${version}"

cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity="https://github.com/ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml@refs/tags/v${version}" \
  --certificate-oidc-issuer='https://token.actions.githubusercontent.com'

sha256sum --ignore-missing --check SHA256SUMS
tar -xzf "${archive}"
install -d "${HOME}/.local/bin"
install -m 755 zebrad "${HOME}/.local/bin/zebrad"
export PATH="${HOME}/.local/bin:${PATH}"
zebrad --version
```

Stop if any verification fails. The attestation binds the archive to the
expected repository, builder workflow, and release tag. Cosign authenticates
the checksum manifest; the checksum check then compares the downloaded
archive with that signed manifest. An unsigned checksum alone detects file
corruption but does not authenticate the publisher.

An attestation is not an [independent rebuild](https://github.com/ZcashFoundation/zebra/blob/main/docs/decisions/devops/0007-reproducible-builds.md).

Keep `${HOME}/.local/bin` in the service or shell's `PATH`, or invoke the
installed binary by its full path. Continue with [running Zebra](run.md).

<a id="building-zebra"></a>

## Building from source

The `zebrad` binary requires Rust 1.91 or newer; Zebra 7.0.0 pins Rust 1.91.0
in `rust-toolchain.toml`. Install [rustup](https://www.rust-lang.org/tools/install),
then select the release toolchain:

```sh
rustup toolchain install 1.91.0
rustup default 1.91.0
```

The bundled RocksDB and Zcash script library require a C compiler, a C++17
compiler, and Clang's shared library, `libclang`, for generated Rust bindings.
Install Git for the tagged-source procedure below.

The [RPC build script](https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/build.rs)
uses checked-in generated files unless the Protocol Buffers compiler, `protoc`,
and source `.proto` files are available. Install `protoc` only to regenerate
these files or build with the `lightwalletd-grpc-tests` feature:

| Platform | Package |
| --- | --- |
| Debian / Ubuntu | `protobuf-compiler` |
| Arch Linux / Homebrew | `protobuf` |

### Debian and Ubuntu

```sh
sudo apt-get update
sudo apt-get install build-essential clang libclang-dev git
```

### Arch Linux

```sh
sudo pacman -Syu --needed base-devel clang git
```

### Fedora and RHEL

On Fedora:

```sh
sudo dnf install gcc gcc-c++ make clang clang-libs clang-devel git
```

On RHEL 9, install the LLVM toolset and libclang runtime from the enabled RHEL
repositories:

```sh
sudo dnf install gcc gcc-c++ make llvm-toolset clang-libs git
```

### macOS

Install the Xcode Command Line Tools for the compiler and SDK, and
[Homebrew LLVM](https://formulae.brew.sh/formula/llvm) for libclang:

```sh
xcode-select --install
brew install llvm
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"
```

Complete the Command Line Tools installation before building.

### Install from crates.io

Build and install the latest published release with its dependency lockfile:

```sh
cargo install --locked zebrad
```

To pin Zebra 7.0.0:

```sh
cargo install --locked --version 7.0.0 zebrad
```

Cargo installs the executable in its installation root, normally
`${HOME}/.cargo/bin`. Keep that directory in `PATH`. The `--locked` option
retains the release's dependency versions; do not remove it as a generic
workaround for a build failure.

### Install from a release tag

Build and install directly from the Git tag:

```sh
cargo install --locked \
  --git https://github.com/ZcashFoundation/zebra \
  --tag v7.0.0 zebrad
```

Alternatively, build a checkout without installing it:

```sh
git clone --branch v7.0.0 https://github.com/ZcashFoundation/zebra.git
cd zebra
cargo build --locked --release --bin zebrad
target/release/zebrad --version
```

Run the installed binary with `zebrad start`, or the checkout build with
`target/release/zebrad start`. See [running Zebra](run.md) for configuration,
network selection, and shutdown.

### Dynamic RocksDB linking

The default build statically links bundled RocksDB. To avoid recompiling it,
link an installed shared library instead.

Zebra 7.0.0 locks `rocksdb` to 0.24.0 and `librocksdb-sys` to
0.17.3+10.4.2, which bundles RocksDB **10.4.2**. Use matching 10.4.2 headers
and a shared library built with LZ4 support. Do not assume that an arbitrary
system package supplies the required API; the build script does not validate
the library's version.

For a matching installation under `/opt/rocksdb-10.4.2`, with libraries in
`lib` and headers in `include`:

```sh
unset ROCKSDB_STATIC ROCKSDB_COMPILE
export ROCKSDB_LIB_DIR=/opt/rocksdb-10.4.2/lib
export ROCKSDB_INCLUDE_DIR=/opt/rocksdb-10.4.2/include
cargo build --locked --release --bin zebrad
```

The [`librocksdb-sys` build script](https://docs.rs/crate/librocksdb-sys/0.17.3+10.4.2/source/build.rs)
uses `ROCKSDB_LIB_DIR` to skip the bundled C++ build. It selects dynamic
linking when `ROCKSDB_STATIC` is absent; even `0` selects static linking.
The `ROCKSDB_INCLUDE_DIR` variable selects bindgen's headers; libclang is
still required. Set both paths to the actual installation.

Configure the operating system's dynamic-library loader to find RocksDB and
its dependencies when Zebra starts. The resulting binary depends on those
libraries remaining installed. Unset `ROCKSDB_LIB_DIR` and
`ROCKSDB_INCLUDE_DIR` to return to the bundled build.

### Building on ARM

Use a native 64-bit ARM Rust toolchain and native C/C++ dependencies:
`aarch64-unknown-linux-gnu` for Linux or `aarch64-apple-darwin` for Apple
silicon. Do not select an x86-64 toolchain. There is no official 32-bit ARM
archive.

### Build troubleshooting

| Failure | Action |
| --- | --- |
| Cargo rejects the Rust version | Select Rust 1.91.0 or newer. Check for a `RUSTUP_TOOLCHAIN` override or a different `cargo` earlier in `PATH`. |
| Bindgen cannot find `libclang.so` or `libclang.dylib` | Install the Clang shared library and set `LIBCLANG_PATH` to its containing directory if automatic discovery fails. A compiler executable alone is not sufficient. |
| The loader cannot open a versioned libclang library | Install the matching runtime package. Pointing at headers or a different library version does not repair a missing runtime dependency. |
| The C/C++ compiler or linker is missing | Install the native build-tool packages above. Select a compiler explicitly with `CC` and `CXX` when necessary. |
| The build cannot find `protoc` | Install it when using `lightwalletd-grpc-tests`. Remove an invalid `PROTOC` override; a normal RPC build falls back to generated files only when no compiler is selected or found. |
| Dynamic RocksDB linking reports missing symbols or Zebra cannot load RocksDB | Check that the headers, shared library, and loader paths refer to the same compatible installation, or return to the bundled build. |
| Compilation exhausts memory | Reduce Cargo's parallel jobs, for example `cargo build --locked --release --bin zebrad --jobs 1`. This changes build concurrency, not node runtime requirements. |

#### Tor feature

Zebra 7.0.0 has no buildable `zebra-network/tor` feature; selecting it fails
Cargo feature resolution. See [Arti support](https://github.com/ZcashFoundation/zebra/issues/8328).

## Cargo features

Cargo features select code at build time; configuration selects runtime
behavior. The table covers every explicit feature in
[`zebrad/Cargo.toml`](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/Cargo.toml#L63-L157).
“Default” includes features enabled transitively by `default`.
Mining RPCs require no optional Cargo feature; `internal-miner` adds the
built-in solver, not the mining API.

| Feature | Default | Purpose | Runtime prerequisites or configuration |
| --- | --- | --- | --- |
| `default` | Yes | Production feature selector; enables `default-release-binaries`. | Each included component retains its own runtime activation requirements. |
| `default-release-binaries` | Yes | Production bundle: `release_max_level_info`, `progress-bar`, `prometheus`, `sentry`, and `opentelemetry`. | The archive workflow explicitly selects this bundle. |
| `indexer` | No | Production integration: adds state indexes mapping spent outputs and revealed nullifiers to spending transactions. | No runtime switch; opening a populated state adds the indexes. A build without the feature removes them. The indexer gRPC listener is separate and uses `[rpc] indexer_listen_addr` without requiring this feature. |
| `internal-miner` | No | Experimental CPU Equihash miner; runs one solver and checks the target difficulty. | Set `[mining] miner_address` and `internal_miner = true`; see [internal miner setup](regtest.md#using-zebras-internal-miner). Use generation RPCs, not this solver, for deterministic Regtest block production. |
| `elasticsearch` | No | Experimental integration: exports finalized block data to Elasticsearch. | Requires an Elasticsearch service and `[state] elasticsearch_url`, `elasticsearch_username`, and `elasticsearch_password`; see [Elasticsearch](elasticsearch.md). |
| `sentry` | Yes | Production monitoring: sends diagnostic events to Sentry. | Set `SENTRY_DSN` to activate; see [Sentry monitoring](tracing.md#sentry-production-monitoring). |
| `journald` | No | Production logging: writes tracing events to systemd-journald. | Requires Linux with a reachable journal and `[tracing] use_journald = true`. |
| `filter-reload` | No | Debugging and operations: changes tracing filters through HTTP. | Set `[tracing] endpoint_addr`. The endpoint is unauthenticated; restrict it to trusted access. It cannot restore logs removed at compile time. |
| `opentelemetry` | Yes | Production observability: exports traces using OpenTelemetry. | Set `[tracing] opentelemetry_endpoint` or `OTEL_EXPORTER_OTLP_ENDPOINT` to an OTLP collector; see [trace export](tracing.md#opentelemetry-export). |
| `progress-bar` | Yes | Operator display: renders synchronization, mempool, and network progress. | Set `[tracing] progress_bar` to `"summary"` or `"detailed"`. Zebra redirects logs to a file when progress display is active. |
| `prometheus` | Yes | Production monitoring: exposes Prometheus metrics. | Set `[metrics] endpoint_addr`; see [metrics](metrics.md). No listener is enabled by default. |
| `error-debug` | No | Debugging: enables error caller tracking and span-trace capture through `color-eyre`. | No Zebra configuration switch; error reporting uses the configured tracing context. |
| `release_max_level_warn` | No | Production log-volume control: removes tracing levels more verbose than `warn` in release builds. | Runtime filters can select only levels retained by the build. |
| `release_max_level_info` | Yes | Production logging: removes `debug` and `trace` calls from both `tracing` and `log` in release builds. | Runtime filters cannot enable debug or trace output in the default release binary. |
| `release_max_level_debug` | No | Debugging: retains tracing through `debug`, but removes `trace`, in release builds. | Disable the default info ceiling before selecting it, then configure `[tracing] filter`. |
| `max_level_warn` | No | Production log-volume control: removes tracing levels more verbose than `warn` in every build profile. | No runtime switch can raise this ceiling. |
| `max_level_info` | No | Production log-volume control: removes tracing levels more verbose than `info` in every build profile. | No runtime switch can raise this ceiling. |
| `max_level_debug` | No | Debugging: removes `trace` calls from both `tracing` and `log` in every build profile. | Disable lower default ceilings when debug output is needed. |
| `flamegraph` | No | Debugging: records tracing spans for flamegraph output. | Set `[tracing] flamegraph` to the output path; see [tracing](tracing.md). |
| `proptest-impl` | No | Tests: enables property-test data generators in Zebra crates. | Used by test code; no node configuration switch. |
| `zebra-checkpoints` | No | Tests: enables checkpoint-generation test support and the `zebra-utils/zebra-checkpoints` feature. | Checkpoint tests need a synced cached state and the `zebra-checkpoints` executable; see [checkpoint generation](../dev/zebra-checkpoints.md). |
| `lightwalletd-grpc-tests` | No | Tests: generates the lightwalletd gRPC test client. | Requires `protoc` while building and an installed `lightwalletd` binary for the tests. It is not required to serve normal wallet traffic. |
| `tokio-console` | No | Debugging: enables Tokio task instrumentation through `console-subscriber`. | Build with `RUSTFLAGS="--cfg tokio_unstable"`, disable default log ceilings, and use the `tokio-console` client; see [setup](../dev/tokio-console.md#setup). |
| `comparison-interpreter` | No | Tests: runs the Rust and C++ script interpreters and reports disagreements. | No runtime activation flag; do not use for production. |

Cargo also exposes implicit features for optional dependencies. The default
bundle enables `howudoin`, `indicatif`, and `metrics-exporter-prometheus`.
The remaining dependency features are `console-subscriber`, `inferno`,
`proptest`, `proptest-derive`, `thread-priority`, `tonic-prost-build`,
`tracing-flame`, `tracing-journald`, and `zebra-utils`. Selecting a dependency
alone does not activate the corresponding Zebra feature; use the functional
features in the table.

Add features without removing the default bundle:

```sh
cargo install --locked --version 7.0.0 --features "indexer,journald" zebrad
```

Features are additive. To retain debug logging in a release build, remove the
default info ceiling and select the desired production components explicitly:

```sh
cargo install --locked --version 7.0.0 --no-default-features \
  --features "progress-bar,prometheus,sentry,opentelemetry,release_max_level_debug" \
  zebrad
```

Set `[tracing] filter` to request the retained debug events. Avoid
`--all-features` for an operator build: it includes test features and
incompatible logging/instrumentation choices.
