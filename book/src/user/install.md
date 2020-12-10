# Installing Zebra

Building `zebrad` requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/get_started.html), and a C++ compiler.

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
     - Using `rustup` installs the stable Rust toolchain, which `zebrad` targets.
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **a C++ compiler:** use `clang`, `g++`, `Xcode`, `MSVC`, or similar
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-alpha.0 zebrad`
4. Run `zebrad start`

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

#### Build Troubleshooting

If you're having trouble with:
- **libclang:** check out the [clang-sys documentation](https://github.com/KyleMayes/clang-sys#dependencies)
- **g++ or MSVC++:** try using clang or Xcode instead
- **rustc:** use rustc 1.48 or later
  - Zebra does not have a minimum supported Rust version (MSRV) policy yet

### System Requirements

We usually build `zebrad` on systems with:
- 2+ CPU cores
- 7+ GB RAM
- 14+ GB of disk space

On many-core machines (like, 32-core) the build is very fast; on 2-core machines
it's less fast.

We continuously test that our builds and tests pass on:
- Windows Server 2019
- macOS Big Sur 11.0
- Ubuntu 18.04 / the latest LTS
- Debian Buster

We usually run `zebrad` on systems with:
- 4+ CPU cores
- 16+ GB RAM
- 50GB+ available disk space for finalized state
- 100+ Mbps network connections

`zebrad` might build and run fine on smaller and slower systems - we haven't
tested its exact limits yet.
