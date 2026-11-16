![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)

---

[![Unit Tests](https://github.com/ZcashFoundation/zebra/actions/workflows/tests-unit.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/tests-unit.yml)
[![Lint](https://github.com/ZcashFoundation/zebra/actions/workflows/lint.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/lint.yml)
[![Integration Tests (GCP)](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-ci-integration-tests-gcp.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-ci-integration-tests-gcp.yml)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
[![Build docs](https://github.com/ZcashFoundation/zebra/actions/workflows/book.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/book.yml)
[![Deploy Nodes (GCP)](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-deploy-nodes-gcp.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-deploy-nodes-gcp.yml)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

- [Getting Started](#getting-started)
  - [Docker](#docker)
  - [Manual Install](#manual-install)
- [CI/CD Architecture](#cicd-architecture)
- [Documentation](#documentation)
- [User support](#user-support)
- [Security](#security)
- [License](#license)

[Zebra](https://zebra.zfnd.org/) is a Zcash full node written in Rust.

## Getting Started

You can run Zebra using our [Docker
image](https://hub.docker.com/r/zfnd/zebra/tags) or you can install it manually.

### Docker

This command will run our latest release, and sync it to the tip:

```sh
docker run -d \
  --name zebra \
  -p 8233:8233 \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:latest
```

The `-p 8233:8233` flag exposes the P2P port so other Zcash nodes can connect to
yours, and `-v` persists the chain state across restarts. For Testnet, also set
`-e ZEBRA_NETWORK__NETWORK=Testnet` and publish `-p 18233:18233` instead. For
more information, read our [Docker
documentation](https://zebra.zfnd.org/user/docker.html).

### Manual Install

Building Zebra requires [Rust](https://www.rust-lang.org/tools/install) 1.91
or newer, [libclang](https://clang.llvm.org/doxygen/group__CINDEX.html), and
C/C++17 build tools. Below are quick summaries for installing these dependencies.

[//]: # "The empty lines in the `summary` tag below are required for correct Markdown rendering."

<details><summary>

#### General Instructions for Installing Dependencies

</summary>

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
2. Install Zebra's build dependencies:
   - **libclang**, which is a library that comes under various names, typically
     `libclang`, `libclang-dev`, `llvm`, or `llvm-dev`;
   - **clang** or another C++17 compiler (`g++` on Linux or the
     Xcode Command Line Tools on macOS);
   - **[`protoc`](https://grpc.io/docs/protoc-installation/)** (only for
     regenerating Protocol Buffers code or building lightwalletd gRPC tests).

</details>

[//]: # "The empty lines in the `summary` tag below are required for correct Markdown rendering."

<details><summary>

#### Dependencies on Arch Linux

</summary>

```sh
sudo pacman -Syu --needed rust base-devel clang
```

Installing `clang` also installs its `clang-libs` dependency, including libclang.

</details>

On `x86_64` or `aarch64` Linux (glibc 2.34+ and `libstdc++`), you can skip the
build dependencies and download a pre-built binary with
[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo binstall zebrad
```

The same binaries are attached to each
[GitHub release](https://github.com/ZcashFoundation/zebra/releases), with
SHA-256 checksums and a Cosign-signed checksum manifest. GitHub stores a
build-provenance attestation for each archive. Zebra's cargo-binstall metadata
does not configure signature verification; use the
[manual verification procedure](https://zebra.zfnd.org/user/install.html#download-and-verify-an-archive)
to authenticate these artifacts.

Otherwise, once you have the dependencies in place, you can build and install
Zebra from source with:

```sh
cargo install --locked zebrad
```

Alternatively, you can install it from GitHub:

```sh
cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v7.0.0 zebrad
```

You can start Zebra by running

```sh
zebrad start
```

Refer to the [Building and Installing
Zebra](https://zebra.zfnd.org/user/install.html) and [Running
Zebra](https://zebra.zfnd.org/user/run.html) sections in the book for enabling
optional features, detailed configuration and further details.

## CI/CD Architecture

Zebra uses a comprehensive CI/CD system built on GitHub Actions to ensure code
quality, maintain stability, and automate routine tasks. Our CI/CD
infrastructure:

- Runs automated tests for relevant changes on PRs and supported branches.
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

  - User guides of note:
    - [Zebra Health Endpoints](https://zebra.zfnd.org/user/health.html) — liveness/readiness checks for Kubernetes and load balancers

- The [documentation of the public
  APIs](https://docs.rs/zebrad/latest/zebrad/#zebra-crates) for the latest
  releases of the individual Zebra crates.

- The [documentation of the internal APIs](https://zebra.zfnd.org/internal)
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
