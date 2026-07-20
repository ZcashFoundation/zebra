# Building and Installing Zebra

The easiest way to install and run Zebra is to follow the [Getting
Started](https://zebra.zfnd.org/index.html#getting-started) section.

## Pre-built binaries

Every [GitHub release](https://github.com/ZcashFoundation/zebra/releases) ships
pre-built `zebrad` binaries for Linux on `x86_64` and `aarch64`, as
`zebrad-<version>-<target>.tar.gz` archives. They are built on Ubuntu 22.04 and
need glibc 2.34 or newer (Ubuntu 22.04+, Debian 12+, RHEL 9+, Amazon Linux 2023);
on older or other platforms use the [Docker image](https://hub.docker.com/r/zfnd/zebra)
or build from source.

Install the latest release with
[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall):

```bash
cargo binstall zebrad
```

Or download an archive, verify it, and extract `zebrad`:

```bash
gh attestation verify zebrad-<version>-x86_64-unknown-linux-gnu.tar.gz \
  --repo ZcashFoundation/zebra \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp='^https://github\.com/ZcashFoundation/zebra/\.github/workflows/zfnd-release-binaries\.yml@' \
  --certificate-oidc-issuer='https://token.actions.githubusercontent.com'
sha256sum --ignore-missing -c SHA256SUMS
tar xzf zebrad-<version>-x86_64-unknown-linux-gnu.tar.gz
```

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
  documentation](https://github.com/KyleMayes/clang-sys#dependencies). If the
  build fails with `couldn't find any valid shared libraries matching:
  ['libclang.so', 'libclang-*.so']`, libclang is not installed or is not on the
  search path: install the `libclang-dev` package (Debian/Ubuntu) or set
  `LIBCLANG_PATH` to the directory that contains `libclang.so`.
- **shared libraries:** If the build fails with `error while loading shared
  libraries: libclang-*.so.*: cannot open shared object file`, you have the
  libclang **dev** files but are missing the matching **runtime** library.
  Install the runtime package (`libclang1-<version>` on Debian/Ubuntu) or run
  `sudo ldconfig`.
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
