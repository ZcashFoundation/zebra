# Developer Documentation

This section contains the contribution guide and design documentation. It
does not contain API documentation, which is generated using Rustdoc:

- [`doc.zebra.zfnd.org`](https://doc.zebra.zfnd.org/) renders documentation for the public API;
- [`doc-internal.zebra.zfnd.org`](https://doc-internal.zebra.zfnd.org/) renders documentation for the internal API.

# Building Zebra

Building `zebrad` requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/get_started.html), and a C++ compiler.

#### Detailed Build and Run Instructions

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
     - Using `rustup` installs the stable Rust toolchain, which `zebrad` targets.
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **a C++ compiler:** use `clang`, `g++`, `Xcode`, `MSVC`, or similar
3. Run `git clone https://github.com/ZcashFoundation/zebra`
4. Run `cargo build --workspace`
5. Run `./target/debug/zebrad start`

This will build with the latest dependency versions according to the Cargo.toml files.
If you want to build using the Cargo.lock'd dependency versions, run
`cargo build --workspace --locked`

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

#### Build Troubleshooting

If you're having trouble with:
- **libclang:** check out the [clang-sys documentation](https://github.com/KyleMayes/clang-sys#dependencies)
- **g++ or MSVC++:** try using clang or Xcode instead
- **rustc:** use rustc 1.48 or later
  - Zebra does not have a minimum supported Rust version (MSRV) policy yet
