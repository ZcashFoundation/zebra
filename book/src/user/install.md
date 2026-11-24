# Building and Installing Zebra

The easiest way to install and run Zebra is to follow the [Getting
Started](https://zebra.zfnd.org/index.html#getting-started) section.

## Pre-built binaries

[GitHub releases](https://github.com/ZcashFoundation/zebra/releases) from
v6.0.0 on ship pre-built `zebrad` binaries for Linux on `x86_64` and `aarch64`,
as `zebrad-<version>-<target>.tar.gz` archives. They are built on Ubuntu 22.04 and
need glibc 2.34 or newer (Ubuntu 22.04+, Debian 12+, RHEL 9+, Amazon Linux 2023);
on older or other platforms use the [Docker image](https://hub.docker.com/r/zfnd/zebra)
or build from source.

Some releases have no archives, for example v6.4.0 and v6.4.1. Check that the
release you want lists them under its assets before you follow these steps.

### Verify and extract an archive

Each release with archives also has a `SHA256SUMS` file, signed with Cosign in
`SHA256SUMS.sigstore.json`, and a Sigstore build-provenance attestation for each
archive. Download those two files and the archive for your platform, then
verify them and extract `zebrad`.

The commands need:

- the [GitHub CLI](https://cli.github.com/), signed in with `gh auth login` or
  given a token in `GH_TOKEN`, because `gh attestation verify` fails without one
- [Cosign](https://docs.sigstore.dev/cosign/system_config/installation/) 3.1.3
  or later, because earlier versions can skip the certificate identity and
  issuer checks for a forged bundle
  ([GHSA-fx35-mq7g-6g98](https://github.com/sigstore/cosign/security/advisories/GHSA-fx35-mq7g-6g98))

The commands are joined with `&&`, so `tar` extracts the archive only if every
check passes:

```bash
gh attestation verify zebrad-<version>-x86_64-unknown-linux-gnu.tar.gz \
  --repo ZcashFoundation/zebra \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml \
  --source-ref 'refs/tags/v<version>' &&
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity='https://github.com/ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml@refs/tags/v<version>' \
  --certificate-oidc-issuer='https://token.actions.githubusercontent.com' &&
sha256sum --ignore-missing -c SHA256SUMS &&
tar xzf zebrad-<version>-x86_64-unknown-linux-gnu.tar.gz
```

### Install with `cargo binstall`

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads the
same archive from the GitHub release and installs `zebrad`. It does not check
the Cosign signature or the build-provenance attestation. If you need those
checks, use the steps in the previous section.

```bash
cargo binstall zebrad
```

Releases published with Zebra's updated crate metadata turn off the
`cargo binstall` fallback to third-party
[QuickInstall](https://github.com/cargo-bins/cargo-quickinstall) builds by
default. For those releases, if there is no archive for your platform,
`cargo binstall zebrad` builds Zebra from source instead, which needs the build
dependencies from
[Manual Install](https://zebra.zfnd.org/index.html#manual-install). That crate
setting is only a default: passing `--strategies` or setting
`BINSTALL_STRATEGIES` replaces it, so a list that includes `quick-install` can
still install a QuickInstall build.

Already-published releases without this metadata (such as v7.1.0 and earlier)
can still fall back to QuickInstall when no release archive is available. To
prevent that fallback when installing one of those versions, pass
`--disable-strategies quick-install`:

```bash
cargo binstall --disable-strategies quick-install zebrad
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

#### Tor feature

Experimental Tor support is currently
[disabled](https://github.com/ZcashFoundation/zebra/issues/8328#issuecomment-1969989648)
until Zebra upgrades to the latest `arti-client`, and the `zebra-network/tor`
feature cannot be built.
