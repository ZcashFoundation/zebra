# Installing Zebra

`zebrad` is still under development, so there is no supported packaging or
install mechanism. To run `zebrad`, follow the instructions to compile `zebrad`
for your platform:

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
     - Using `rustup` installs the stable Rust toolchain, which `zebrad` targets.
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **clang** or another C++ compiler: `g++`, `Xcode`, or `MSVC`
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag 1.0.0-beta.4 zebrad`
4. Run `zebrad start` (see [Running Zebra](user/run.md) for more information)

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

#### Build Troubleshooting

If you're having trouble with:
- **dependencies:**
  - install both `libclang` and `clang` - they are usually different packages
  - use `cargo install` without `--locked` to build with the latest versions of each dependency
- **libclang:** check out the [clang-sys documentation](https://github.com/KyleMayes/clang-sys#dependencies)
- **g++ or MSVC++:** try using clang or Xcode instead
- **rustc:** use rustc 1.48 or later
  - Zebra does not have a minimum supported Rust version (MSRV) policy yet

### Dependencies

Zebra primarily depends on pure Rust crates, and some Rust/C++ crates:
- [rocksdb](https://crates.io/crates/rocksdb)
- [zcash_script](https://crates.io/crates/zcash_script)

They will be automatically built along with `zebrad`.
