# Installing Zebra

To install Zebra, follow the [Getting Started](https://zebra.zfnd.org/index.html#getting-started) section.

## Optional Configs & Features

Zebra supports a variety of optional features which you can enable and configure
manually.

### Initializing Configuration File

```console
zebrad generate -o ~/.config/zebrad.toml
```

The above command places the generated `zebrad.toml` config file in the default
preferences directory of Linux. For other OSes default locations [see
here](https://docs.rs/dirs/latest/dirs/fn.preference_dir.html).

### Configuring Progress Bars

Configure `tracing.progress_bar` in your `zebrad.toml` to [show key metrics in
the terminal using progress
bars](https://zfnd.org/experimental-zebra-progress-bars/). When progress bars
are active, Zebra automatically sends logs to a file.

There is a known issue where [progress bar estimates become extremely
large](https://github.com/console-rs/indicatif/issues/556).

In future releases, the `progress_bar = "summary"` config will show a few key
metrics, and the "detailed" config will show all available metrics. Please let
us know which metrics are important to you!

### Configuring Mining

Zebra can be configured for mining by passing a `MINER_ADDRESS` and port mapping
to Docker. See the [mining support
docs](https://zebra.zfnd.org/user/mining-docker.html) for more details.

### Custom Build Features

You can also build Zebra with additional [Cargo
features](https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options):

- `prometheus` for [Prometheus metrics](https://zebra.zfnd.org/user/metrics.html)
- `sentry` for [Sentry monitoring](https://zebra.zfnd.org/user/tracing.html#sentry-production-monitoring)
- `elasticsearch` for [experimental Elasticsearch support](https://zebra.zfnd.org/user/elasticsearch.html)
- `shielded-scan` for [experimental shielded scan support](https://zebra.zfnd.org/user/shielded-scan.html)

You can combine multiple features by listing them as parameters of the
`--features` flag:

```sh
cargo install --features="<feature1> <feature2> ..." ...
```

Our full list of experimental and developer features is in [the API
documentation](https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags).

Some debugging and monitoring features are disabled in release builds to
increase performance.

## Alternative Compilation Methods

Zebra also supports the following compilation methods.

### Compiling Manually from git

To compile Zebra directly from GitHub, or from a GitHub release source archive:

1. Install the dependencies as described in the [Getting
   Started](https://zebra.zfnd.org/index.html#getting-started) section.

2. Get the source code using `git` or from a GitHub source package

```sh
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra
git checkout v2.3.0
```

3. Build and Run `zebrad`

```sh
cargo build --release --bin zebrad
target/release/zebrad start
```

### Compiling from git using cargo install

```sh
cargo install --git https://github.com/ZcashFoundation/zebra --tag v2.3.0 zebrad
```

### Compiling on ARM

If you're using an ARM machine, [install the Rust compiler for
ARM](https://rust-lang.github.io/rustup/installation/other.html). If you build
using the x86_64 tools, Zebra might run really slowly.

## Build Troubleshooting

If you're having trouble with:

### Compilers

- **clang:** install both `libclang` and `clang` - they are usually different packages
- **libclang:** check out the [clang-sys documentation](https://github.com/KyleMayes/clang-sys#dependencies)
- **g++ or MSVC++:** try using clang or Xcode instead
- **rustc:** use the latest stable `rustc` and `cargo` versions
  - Zebra does not have a minimum supported Rust version (MSRV) policy: any release can update the required Rust version.

### Dependencies

- use `cargo install` without `--locked` to build with the latest versions of each dependency

## Experimental Shielded Scanning feature

- install the `rocksdb-tools` or `rocksdb` packages to get the `ldb` binary, which allows expert users to
  [query the scanner database](https://zebra.zfnd.org/user/shielded-scan.html). This binary is sometimes called `rocksdb_ldb`.

## Optional Tor feature

- **sqlite linker errors:** libsqlite3 is an optional dependency of the `zebra-network/tor` feature.
  If you don't have it installed, you might see errors like `note: /usr/bin/ld: cannot find -lsqlite3`.
  [Follow the arti instructions](https://gitlab.torproject.org/tpo/core/arti/-/blob/main/CONTRIBUTING.md#setting-up-your-development-environment)
  to install libsqlite3, or use one of these commands instead:

```sh
cargo build
cargo build -p zebrad --all-features
```

