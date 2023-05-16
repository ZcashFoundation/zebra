![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)

---

[![CI Docker](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-docker.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-docker.yml) [![CI OSes](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-os.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-integration-os.yml) [![Continuous Delivery](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-delivery.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/continous-delivery.yml) [![Coverage](https://github.com/ZcashFoundation/zebra/actions/workflows/coverage.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/coverage.yml) [![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra) [![Build docs](https://github.com/ZcashFoundation/zebra/actions/workflows/docs.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/docs.yml) [![Build lightwalletd](https://github.com/ZcashFoundation/zebra/actions/workflows/zcash-lightwalletd.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/zcash-lightwalletd.yml) [![Build Zcash Params](https://github.com/ZcashFoundation/zebra/actions/workflows/zcash-params.yml/badge.svg)](https://github.com/ZcashFoundation/zebra/actions/workflows/zcash-params.yml)

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

## Contents

- [Contents](#contents)
- [About](#about)
  - [Using Zebra](#using-zebra)
- [Release Candidates](#release-candidates)
- [Getting Started](#getting-started)
  - [Docker](#docker)
  - [Building Zebra](#building-zebra)
    - [Optional Features](#optional-features)
  - [Configuring JSON-RPC for lightwalletd](#configuring-json-rpc-for-lightwalletd)
  - [Network Ports](#network-ports)
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

Zebra aims to be [faster, more secure, and more easily extensible](https://doc.zebra.zfnd.org/zebrad/index.html#zebra-advantages)
than other Zcash implementations.

## Release Candidates

Every few weeks, we release a [new Zebra version](https://github.com/ZcashFoundation/zebra/releases).

Zebra's network stack is interoperable with `zcashd`,
and Zebra implements all the features required to reach Zcash network consensus.
Currently, Zebra validates all of the Zcash consensus rules for the NU5 network upgrade.

Zebra validates blocks and transactions, but needs extra software to generate them:

- to generate transactions, [configure `zebrad`'s JSON-RPC port](https://github.com/ZcashFoundation/zebra#configuring-json-rpc-for-lightwalletd),
  and use a light wallet with `lightwalletd` and Zebra.
- to generate blocks, [compile `zebrad` with the `getblocktemplate-rpcs` feature](https://doc.zebra.zfnd.org/zebrad/#json-rpc), configure the JSON-RPC port,
  and use a mining pool or miner with Zebra's mining JSON-RPCs.
  Mining support is currently incomplete, experimental, and off by default.

## Getting Started

You can run Zebra using our Docker image or you can build it manually. Please
see [the Book](https://zebra.zfnd.org/user/requirements.html) for system
requirements.

### Docker

This command will run our latest release, and sync it to the tip:

```sh
docker run zfnd/zebra:1.0.0-rc.8
```

For more information, read our [Docker documentation](book/src/user/docker.md).

### Building Zebra

Building Zebra requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/doxygen/group__CINDEX.html),
[pkg-config](http://pkgconf.org/), and a C++ compiler.

Zebra is tested with the latest `stable` Rust version. Earlier versions are not
supported or tested. Note that Zebra's code currently uses features introduced
in Rust 1.68, or any later stable release.

Below are quick summaries for installing the dependencies on your machine.

<details><summary><h4>General instructions for installing dependencies</h4></summary>

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).

2. Install Zebra's build dependencies:

   - **libclang** is a library that might have different names depending on your
     package manager. Typical names are `libclang`, `libclang-dev`, `llvm`, or
     `llvm-dev`.
   - **clang** or another C++ compiler: `g++` (all platforms) or `Xcode` (macOS).
   - **pkg-config**

</details>

<details><summary><h4>Dependencies on Arch</h4></summary>

```sh
sudo pacman -S rust clang pkgconf
```

Note that the package `clang` includes `libclang` as well as the C++ compiler.

</details>

Once the dependencies are in place, you can build Zebra

```sh
cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-rc.8 zebrad
```

You can start Zebra by

```sh
zebrad start
```

See the [Running Zebra](https://zebra.zfnd.org/user/run.html) section in the
book for more details.

#### Optional Features

You can also build Zebra with the following [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options):

- `sentry` for [Sentry monitoring](https://zebra.zfnd.org/user/requirements.html#sentry-production-monitoring);
- `filter-reload` for [dynamic tracing](https://zebra.zfnd.org/user/tracing.html#dynamic-tracing)
- `journald` for [`journald` logging](https://zebra.zfnd.org/user/tracing.html#journald-logging).
- `flamegraph` for [generating flamegraphs](https://zebra.zfnd.org/user/tracing.html#flamegraphs).
- `prometheus` for [Prometheus metrics](https://doc.zebra.zfnd.org/zebrad/#metrics).
- `getblocktemplate-rpcs` for [mining support](https://zebra.zfnd.org/user/mining.html).

You can arbitrarily combine the features by listing them as parameters of the `--features` flag:

```sh
cargo install --features="<feature1> <feature2> ..." ...
```

The features are also described in [the API
documentation](https://doc.zebra.zfnd.org/zebrad/index.html#zebra-feature-flags).
The debugging and monitoring features are disabled in release builds to increase
performance.

### Configuring JSON-RPC for lightwalletd

To use `zebrad` as a `lightwalletd` backend, give it this `~/.config/zebrad.toml`:

```toml
[rpc]
# listen for RPC queries on localhost
listen_addr = '127.0.0.1:8232'

# automatically use multiple CPU threads
parallel_cpu_threads = 0
```

**WARNING:** This config allows multiple Zebra instances to share the same RPC port.
See the [RPC config documentation](https://doc.zebra.zfnd.org/zebra_rpc/config/struct.Config.html) for details.

`lightwalletd` also requires a `zcash.conf` file.

It is recommended to use [adityapk00/lightwalletd](https://github.com/adityapk00/lightwalletd) because that is used in testing.
Other `lightwalletd` forks have limited support, see the [detailed `lightwalletd` instructions](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/lightwalletd.md#sync-lightwalletd).

### Network Ports

Zebra uses the following inbound and outbound TCP ports:

- 8233 on Mainnet
- 18233 on Testnet

Please see the [Network
Requirements](https://zebra.zfnd.org/user/requirements.html#network-requirements-and-ports)
section for more details.

## Known Issues

There are a few bugs in Zebra that we're still working on fixing:

- If Zebra fails downloading the Zcash parameters, use [the Zcash parameters download script](https://github.com/zcash/zcash/blob/master/zcutil/fetch-params.sh) instead.

- Block download and verification sometimes times out during Zebra's initial sync [#5709](https://github.com/ZcashFoundation/zebra/issues/5709). The full sync still finishes reasonably quickly.

- No Windows support [#3801](https://github.com/ZcashFoundation/zebra/issues/3801). We used to test with Windows Server 2019, but not any more; see the issue for details.

- Experimental Tor support is disabled until [Zebra upgrades to the latest `arti-client`](https://github.com/ZcashFoundation/zebra/issues/5492). This happened due to a Rust dependency conflict, which could only be resolved by `arti` upgrading to a version of `x25519-dalek` with the dependency fix.

- Output of `help`, `--help` flag, and usage of invalid commands or options are inconsistent [#5502](https://github.com/ZcashFoundation/zebra/issues/5502). See the issue for details.

## Future Work

Performance and Reliability:

- Reliable syncing under poor network conditions
- Additional batch verification
- Performance tuning

Currently, the following features are out of scope:

- Optional Zcash network protocol messages
- Consensus rules removed before Canopy activation (Zebra checkpoints on Canopy activation)

## Documentation

The [Zebra website](https://zebra.zfnd.org/) contains user documentation, such
as how to run or configure Zebra, set up metrics integrations, etc., as well as
developer documentation, such as design documents. We also render [API
documentation](https://doc.zebra.zfnd.org) for the external API of our crates,
as well as [internal documentation](https://doc-internal.zebra.zfnd.org) for
private APIs.

## Security

Zebra has a [responsible disclosure policy](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md), which we encourage security researchers to follow.

## License

Zebra is distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
