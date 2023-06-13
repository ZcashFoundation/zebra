# Installing Zebra

Follow the [Docker or compilation instructions](https://zebra.zfnd.org/index.html#getting-started).

#### ARM

If you're using an ARM machine, [install the Rust compiler for
ARM](https://rust-lang.github.io/rustup/installation/other.html). If you build
using the x86_64 tools, Zebra might run really slowly.

#### Build Troubleshooting

If you're having trouble with:

Dependencies:

- use `cargo install` without `--locked` to build with the latest versions of each dependency
- **sqlite linker errors:** libsqlite3 is an optional dependency of the `zebra-network/tor` feature.
  If you don't have it installed, you might see errors like `note: /usr/bin/ld: cannot find -lsqlite3`.
  [Follow the arti instructions](https://gitlab.torproject.org/tpo/core/arti/-/blob/main/CONTRIBUTING.md#setting-up-your-development-environment)
  to install libsqlite3, or use one of these commands instead:

```sh
cargo build
cargo build -p zebrad --all-features
```

Compilers:

- **clang:** install both `libclang` and `clang` - they are usually different packages
- **libclang:** check out the [clang-sys documentation](https://github.com/KyleMayes/clang-sys#dependencies)
- **g++ or MSVC++:** try using clang or Xcode instead
- **rustc:** use rustc 1.65 or later
  - Zebra does not have a minimum supported Rust version (MSRV) policy: any release can update the required Rust version.

### Dependencies

Zebra primarily depends on pure Rust crates, and some Rust/C++ crates:

- [rocksdb](https://crates.io/crates/rocksdb)
- [zcash_script](https://crates.io/crates/zcash_script)

They will be automatically built along with `zebrad`.
