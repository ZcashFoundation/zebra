# Installing Zebra

`zebrad` is still under development, so there is no supported packaging or
install mechanism. To run `zebrad`, follow the instructions to compile `zebrad`
for your platform:

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
     - Using `rustup` installs the stable Rust toolchain, which `zebrad` targets.
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **clang** or another C++ compiler: `g++`, `Xcode`, or `MSVC`
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-beta.13 zebrad`
4. Run `zebrad start` (see [Running Zebra](run.md) for more information)

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

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
- **rustc:** use rustc 1.58 or later
  - Zebra does not have a minimum supported Rust version (MSRV) policy yet


### Dependencies

Zebra primarily depends on pure Rust crates, and some Rust/C++ crates:
- [rocksdb](https://crates.io/crates/rocksdb)
- [zcash_script](https://crates.io/crates/zcash_script)

They will be automatically built along with `zebrad`.
