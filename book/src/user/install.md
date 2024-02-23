# Installing Zebra

Follow the [Docker or compilation instructions](https://zebra.zfnd.org/index.html#getting-started).

## Installing Dependencies

To compile Zebra from source, you will need to [install some dependencies.](https://zebra.zfnd.org/index.html#building-zebra).

## Alternative Compilation Methods

### Compiling Manually from git

To compile Zebra directly from GitHub, or from a GitHub release source archive:

1. Install the dependencies (see above)

2. Get the source code using `git` or from a GitHub source package

```sh
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra
git checkout v1.6.0
```

3. Build and Run `zebrad`

```sh
cargo build --release --bin zebrad
target/release/zebrad start
```

### Compiling from git using cargo install

```sh
cargo install --git https://github.com/ZcashFoundation/zebra --tag v1.6.0 zebrad
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


