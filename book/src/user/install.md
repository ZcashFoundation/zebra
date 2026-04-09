# Building and Installing Zebra

The easiest way to install and run Zebra is to follow the [Getting
Started](https://zebra.zfnd.org/index.html#getting-started) section.

## Building Zebra

If you want to build Zebra, install the build dependencies as described in the
[Manual Install](https://zebra.zfnd.org/index.html#manual-install) section, and
get the source code from GitHub:

```bash
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra
```

You can then build and run `zebrad` by:

```bash
cargo build --release --bin zebrad
target/release/zebrad start
```

If you rebuild Zebra often, you can speed the build process up by dynamically
linking RocksDB, which is a C++ dependency, instead of rebuilding it and linking
it statically. If you want to utilize dynamic linking, first install RocksDB
version >= 8.9.1 as a system library. On Arch Linux, you can do that by:

```bash
pacman -S rocksdb
```

On Ubuntu version >= 24.04, that would be

```bash
apt install -y librocksdb-dev
```

Once you have the library installed, set

```bash
export ROCKSDB_LIB_DIR="/usr/lib/"
```

and enjoy faster builds. Dynamic linking will also decrease the size of the
resulting `zebrad` binary in release mode by ~ 6 MB.

### Building on ARM

If you're using an ARM machine, install the [Rust compiler for
ARM](https://rust-lang.github.io/rustup/installation/other.html). If you build
Zebra using the `x86_64` tools, it might run really slowly.

### Build Troubleshooting

If you are having trouble with:

- **clang:** Install both `libclang` and `clang` - they are usually different
  packages.
- **libclang:** Check out the [clang-sys
  documentation](https://github.com/KyleMayes/clang-sys#dependencies).
- **g++ or MSVC++:** Try using `clang` or `Xcode` instead.
- **rustc:** Use the latest stable `rustc` and `cargo` versions.
- **dependencies**: Use `cargo install` without `--locked` to build with the
  latest versions of each dependency.

#### Optional Tor feature

The `zebra-network/tor` feature has an optional dependency named `libsqlite3`.
If you don't have it installed, you might see errors like `note: /usr/bin/ld:
cannot find -lsqlite3`. Follow [the arti
instructions](https://gitlab.torproject.org/tpo/core/arti/-/blob/main/CONTRIBUTING.md#setting-up-your-development-environment)
to install `libsqlite3`, or use one of these commands instead:

```sh
cargo build
cargo build -p zebrad --all-features
```
