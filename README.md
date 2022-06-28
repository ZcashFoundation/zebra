![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)

---

[![](https://github.com/ZcashFoundation/zebra/workflows/CI/badge.svg?branch=main)](https://github.com/ZcashFoundation/zebra/actions?query=workflow%3ACI+branch%3Amain)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

## Contents

- [Contents](#contents)
- [About](#about)
- [Beta Releases](#beta-releases)
- [Getting Started](#getting-started)
- [Known Issues](#known-issues)
- [Future Work](#future-work)
- [Documentation](#documentation)
- [Security](#security)
- [License](#license)

## About

[Zebra](https://zebra.zfnd.org/) is the Zcash Foundation's independent,
consensus-compatible implementation of a Zcash node, currently under
development. It can be used to join the Zcash peer-to-peer network, which helps
keeping Zcash working by validating and broadcasting transactions, and maintaining
the Zcash blockchain state in a distributed manner.

[Zcash](https://doc.zebra.zfnd.org/zebrad/index.html#about-zcash)
is a cryptocurrency designed to preserve the user's privacy.
If you just want to send and receive Zcash then you don't need to use Zebra
directly. You can download a Zcash wallet application which will handle that
for you.

Please [join us on Discord](https://discord.gg/na6QZNd) if you'd
like to find out more or get involved!

### Using Zebra

You would want to run Zebra if you want to contribute to the
Zcash network: the more nodes are run, the more reliable the network will be
in terms of speed and resistance to denial of service attacks, for example.

Zebra aims to be
[faster, more secure, and more easily extensible](https://doc.zebra.zfnd.org/zebrad/index.html#zebra-advantages)
than other Zcash implementations.

## Beta Releases

Every few weeks, we release a new Zebra beta [release](https://github.com/ZcashFoundation/zebra/releases).

Zebra's network stack is interoperable with `zcashd`,
and Zebra implements all the features required to reach Zcash network consensus.

Currently, Zebra validates all of the Zcash consensus rules for the NU5 network upgrade.

But it may not validate any:
- Undocumented rules derived from Bitcoin
- Undocumented network protocol requirements

## Getting Started

Building `zebrad` requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/get_started.html), and a C++ compiler.

### Build and Run Instructions

`zebrad` is still under development, so there is no supported packaging or
install mechanism. To run `zebrad`, follow the instructions to compile `zebrad`
for your platform:

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
     - Zebra is tested with the latest `stable` Rust version.
        Earlier versions are not supported or tested, but they might work.
        (Rust 1.57 and earlier are not supported, due to missing features.)
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **clang** or another C++ compiler: `g++`, `Xcode`, or `MSVC`
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-beta.12 zebrad`
4. Run `zebrad start` (see [Running Zebra](https://zebra.zfnd.org/user/run.html) for more information)

For more detailed instructions, refer to the [documentation](https://zebra.zfnd.org/user/install.html).

### Optional Features

For performance reasons, some debugging and monitoring features are disabled in release builds.

You can [enable these features](https://doc.zebra.zfnd.org/zebrad/index.html#zebra-feature-flags) using:
```sh
cargo install --features=<name> ...
```

### System Requirements

The recommended requirements for compiling and running `zebrad` are:
- 4+ CPU cores
- 16+ GB RAM
- 50GB+ available disk space for building binaries and storing finalized state
- 100+ Mbps network connections

We continuously test that our builds and tests pass on:

The *latest* [GitHub Runners](https://docs.github.com/en/actions/using-github-hosted-runners/about-github-hosted-runners#supported-runners-and-hardware-resources) for:
- macOS
- Ubuntu

Docker:
- Debian Bullseye

Zebra's tests can take over an hour, depending on your machine.
We're working on making them faster.

`zebrad` might build and run fine on smaller and slower systems - we haven't
tested its exact limits yet.

For more detailed requirements, refer to the [documentation](https://zebra.zfnd.org/user/requirements.html).

#### Memory Troubleshooting

If Zebra's build runs out of RAM, try setting:
`export CARGO_BUILD_JOBS=2`

If Zebra's tests timeout or run out of RAM, try running:
`cargo test -- --test-threads=2`

(cargo uses all the processor cores on your machine by default.)

#### macOS Test Troubleshooting

Some of Zebra's tests deliberately cause errors that make Zebra panic.
macOS records these panics as crash reports.

If you are seeing "Crash Reporter" dialogs during Zebra tests,
you can disable them using this Terminal.app command:
```sh
defaults write com.apple.CrashReporter DialogType none
```

### Network Ports and Data Usage

By default, Zebra uses the following inbound TCP listener ports:
- 8233 on Mainnet
- 18233 on Testnet

Zebra needs some peers which have a round-trip latency of 2 seconds or less.
If this is a problem for you, please
[open a ticket.](https://github.com/ZcashFoundation/zebra/issues/new/choose)

`zebrad`'s typical mainnet network usage is:
- Initial sync: 31 GB download
- Ongoing updates: 10-100 MB upload and download per day, depending on peer requests

Zebra also performs an initial sync every time its internal database version changes.

For more detailed information, refer to the [documentation](https://zebra.zfnd.org/user/run.html).

#### Network Troubleshooting

Some of Zebra's tests download Zcash blocks, so they might be unreliable depending on your network connection.
You can set `ZEBRA_SKIP_NETWORK_TESTS=1` to skip the network tests.

Zebra may be unreliable on Testnet, and under less-than-perfect network conditions.
See our [roadmap](#future-work) for details.

### Disk Usage

Zebra uses up to 40 GB of space for cached mainnet data,
and 10 GB of space for cached testnet data.

RocksDB cleans up outdated data periodically,
and when the database is closed and re-opened.

#### Disk Troubleshooting

Zebra's state commits changes using RocksDB database transactions.

If you forcibly terminate Zebra, or it panics,
any incomplete changes will be rolled back the next time it starts.

So Zebra's state should always be valid, unless your OS or disk hardware is corrupting data.

## Known Issues

There are a few bugs in Zebra that we're still working on fixing:
- [No Windows support #3801](https://github.com/ZcashFoundation/zebra/issues/3801)
  - We used to test with Windows Server 2019, but not anymore; see issue for details

## Future Work

Features:
- Wallet functionality

Performance and Reliability:
- Reliable syncing under poor network conditions
- Additional batch verification
- Performance tuning

Currently, the following features are out of scope:
- Mining support
- Optional Zcash network protocol messages
- Consensus rules removed before Canopy activation (Zebra checkpoints on Canopy activation)

## Documentation

The [Zebra website](https://zebra.zfnd.org/) contains user documentation, such
as how to run or configure Zebra, set up metrics integrations, etc., as well as
developer documentation, such as design documents.  We also render [API
documentation](https://doc.zebra.zfnd.org) for the external API of our crates,
as well as [internal documentation](https://doc-internal.zebra.zfnd.org) for
private APIs.

## Security

Zebra has a [responsible disclosure policy](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md), which we encourage security researchers to follow.

## License

Zebra is distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
