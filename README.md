![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)

---

[![Integration Tests](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-tests.yml)
[![CI OSes](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-unit-tests-os.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/ci-unit-tests-os.yml)
[![Continuous Delivery](https://github.com/ZcashFoundation/zebra/actions/workflows/cd-deploy-nodes-gcp.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/cd-deploy-nodes-gcp.yml)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
[![Build docs](https://github.com/ZcashFoundation/zebra/actions/workflows/docs-deploy-firebase.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/docs-deploy-firebase.yml)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

- [Getting Started](#getting-started)
  - [Docker](#docker)
  - [Manual Build](#manual-build)
- [CI/CD Architecture](#cicd-architecture)
- [Documentation](#documentation)
- [User support](#user-support)
- [Security](#security)
- [License](#license)

[Zebra](https://zebra.zfnd.org/) is a Zcash full-node written in Rust.

## Getting Started

You can run Zebra using our [Docker
image](https://hub.docker.com/r/zfnd/zebra/tags) or you can build it manually.

### Docker

This command will run our latest release, and sync it to the tip:

```sh
docker run zfnd/zebra:latest
```

For more information, read our [Docker documentation](https://zebra.zfnd.org/user/docker.html).

### Manual Build

Building Zebra requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/doxygen/group__CINDEX.html), and a C++
compiler. Below are quick summaries for installing the dependencies.

[//]: # "The empty line in the `summary` tag below is required for correct Markdown rendering."
<details><summary>

#### General instructions for installing dependencies

</summary>

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).

2. Install Zebra's build dependencies:

   - **libclang** is a library that might have different names depending on your
     package manager. Typical names are `libclang`, `libclang-dev`, `llvm`, or
     `llvm-dev`.
   - **clang** or another C++ compiler: `g++` (all platforms) or `Xcode` (macOS).
   - **[`protoc`](https://grpc.io/docs/protoc-installation/)**

> [!NOTE]
> Zebra uses the `--experimental_allow_proto3_optional` flag with `protoc`
> during compilation. This flag was introduced in [Protocol Buffers
> v3.12.0](https://github.com/protocolbuffers/protobuf/releases/tag/v3.12.0)
> released in May 16, 2020, so make sure you're not using a version of `protoc`
> older than 3.12.

</details>

[//]: # "The empty line in the `summary` tag below is required for correct Markdown rendering."
<details><summary>

#### Dependencies on Arch

</summary>

```sh
sudo pacman -S rust clang protobuf
```

Note that the package `clang` includes `libclang` as well as the C++ compiler.

Recently the GCC version on Arch has broken a build script in the `rocksdb` dependency. A workaround is:

```sh
export CXXFLAGS="$CXXFLAGS -include cstdint"
```

</details>

Once you have the dependencies in place, you can build and install Zebra with:

```sh
cargo install --locked zebrad
```

You can start Zebra by running

```sh
zebrad start
```

Refer to the [Installing Zebra](https://zebra.zfnd.org/user/install.html) and
[Running Zebra](https://zebra.zfnd.org/user/run.html) sections in the book for
enabling optional features, detailed configuration and further details.

## CI/CD Architecture

Zebra uses a comprehensive CI/CD system built on GitHub Actions to ensure code
quality, maintain stability, and automate routine tasks. Our CI/CD
infrastructure:

- Runs automated tests on every PR and commit.
- Manages deployments to various environments.
- Handles cross-platform compatibility checks.
- Automates release processes.

For a detailed understanding of our CI/CD system, including workflow diagrams,
infrastructure details, and best practices, see our [CI/CD Architecture
Documentation](.github/workflows/README.md).

## Documentation

The Zcash Foundation maintains the following resources documenting Zebra:

- The Zebra Book:
  - [General Introduction](https://zebra.zfnd.org/index.html),
  - [User Documentation](https://zebra.zfnd.org/user.html),
  - [Developer Documentation](https://zebra.zfnd.org/dev.html).

- The [documentation of the public
  APIs](https://docs.rs/zebrad/latest/zebrad/#zebra-crates) for the latest
  releases of the individual Zebra crates.

- The [documentation of the internal APIs](https://doc-internal.zebra.zfnd.org)
  for the `main` branch of the whole Zebra monorepo.

## User support

If Zebra doesn't behave the way you expected, [open an
issue](https://github.com/ZcashFoundation/zebra/issues/new/choose). We regularly
triage new issues and we will respond. We maintain a list of known issues in the
[Troubleshooting](https://zebra.zfnd.org/user/troubleshooting.html) section of
the book.

If you want to chat with us, [Join the Zcash Foundation Discord
Server](https://discord.com/invite/aRgNRVwsM8) and find the "zebra-support"
channel.

## Security

Zebra has a [responsible disclosure
policy](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md), which
we encourage security researchers to follow.

## License

Zebra is distributed under the terms of both the MIT license and the Apache
License (Version 2.0). Some Zebra crates are distributed under the [MIT license
only](LICENSE-MIT), because some of their code was originally from MIT-licensed
projects. See each crate's directory for details.

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
