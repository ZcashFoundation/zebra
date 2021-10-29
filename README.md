![Zebra logotype](https://www.zfnd.org/images/zebra-logotype.png)

---

[![](https://github.com/ZcashFoundation/zebra/workflows/CI/badge.svg?branch=main)](https://github.com/ZcashFoundation/zebra/actions?query=workflow%3ACI+branch%3Amain)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

## Contents

- [Contents](#contents)
- [About](#about)
- [Alpha Releases](#alpha-releases)
- [Getting Started](#getting-started)
- [Current Features](#current-features)
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
Please [join us on Discord](https://discord.gg/na6QZNd) if you'd
like to find out more or get involved!

Zcash is a cryptocurrency designed to preserve the user's privacy. Like most
cryptocurrencies, it works by a collection of software nodes run by members of
the Zcash community or any other interested parties. The nodes talk to each
other in peer-to-peer fashion in order to maintain the state of the Zcash
blockchain. They also communicate with miners who create news blocks. When a
Zcash user sends Zcash, their wallet broadcasts transactions to these nodes
which will eventually reach miners, and the mined transaction will then go
through Zcash nodes until they reach the recipient's wallet which will report
the received Zcash to the recipient.

The original Zcash node is named `zcashd` and is developed by the Electric Coin
Company as a fork of the original Bitcoin node. Zebra, on the other hand, is
an independent Zcash node implementation developed from scratch. Since they
implement the same protocol, `zcashd` and Zebra nodes can communicate with each
other.

If you just want to send and receive Zcash then you don't need to use Zebra
directly. You can download a Zcash wallet application which will handle that
for you. (Eventually, Zebra can be used by wallets to implement their
functionality.) You would want to run Zebra if you want to contribute to the
Zcash network: the more nodes are run, the more reliable the network will be
in terms of speed and resistance to denial of service attacks, for example.

These are some of advantages or benefits of Zebra:

- Better performance: since it was implemented from scratch, Zebra was able to be
  implemented in a manner that is currently faster than `zcashd`.
- Better security: since it is developed in a memory-safe language (Rust), Zebra
  is less likely to be affected by security bugs that could compromise the
  environment where it is run.
- Better governance: with a new node deployment, there will be more developers
  who can implement different features.
- Dev accessibility: there will be more developers which gives new developers
  options for contributing to protocol development.
- Runtime safety: the detection of consensus bugs can happen quicker, preventing
  the likelihood of black swan events.
- Spec safety: with several node implementations, it is much easier to notice
  bugs and ambiguity in protocol specification.
- User options: different nodes present different features and tradeoffs for
  users to decide on their preferred options.
- Additional contexts: wider target deployments for people to use a consensus
  node in more contexts e.g. mobile, wasm, etc.

## Alpha Releases

Every few weeks, we release a new Zebra alpha release.

The goals of the alpha release series are to:
- participate in the Zcash network,
- replicate the Zcash chain state,
- implement the Zcash proof of work consensus rules, and
- sync on Mainnet under excellent network conditions.

Currently, Zebra does not validate all the Zcash consensus rules. It may be
unreliable on Testnet, and under less-than-perfect network conditions. See
our [current features](#current-features) and [roadmap](#future-work) for
details.

## Getting Started

Building `zebrad` requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/get_started.html), and a C++ compiler.

### Build and Run Instructions

`zebrad` is still under development, so there is no supported packaging or
install mechanism. To run `zebrad`, follow the instructions to compile `zebrad`
for your platform:

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install).
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **clang** or another C++ compiler: `g++`, `Xcode`, or `MSVC`
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-alpha.19 zebrad`
4. Run `zebrad start` (see [Running Zebra](user/run.md) for more information)

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

For more detailed instructions, refer to the [documentation](https://zebra.zfnd.org/user/install.html).

### System Requirements

The recommended requirements for compiling and running `zebrad` are:
- 4+ CPU cores
- 16+ GB RAM
- 50GB+ available disk space for finalized state
- 100+ Mbps network connections

We continuously test that our builds and tests pass on:
- Windows Server 2019
- macOS Big Sur 11.0
- Ubuntu 18.04 / the latest LTS
- Debian Buster

`zebrad` might build and run fine on smaller and slower systems - we haven't
tested its exact limits yet.

For more detailed requirements, refer to the [documentation](https://zebra.zfnd.org/user/requirements.html).

### Network Ports and Data Usage

By default, Zebra uses the following inbound TCP listener ports:
- 8233 on Mainnet
- 18233 on Testnet

`zebrad`'s typical network usage is:
- initial sync: 30 GB download
- ongoing updates: 10-50 MB upload and download per day, depending on peer requests

For more detailed information, refer to the [documentation](https://zebra.zfnd.org/user/run.html).

## Current Features

Network:
- synchronize the chain from peers
- download gossiped blocks from peers
- answer inbound peer requests for hashes, headers, and blocks

State:
- persist block, transaction, UTXO, and nullifier indexes
- handle chain reorganizations

Proof of Work:
- validate equihash, block difficulty threshold, and difficulty adjustment
- validate transaction merkle roots

Validating proof of work increases the cost of creating a consensus split
between `zebrad` and `zcashd`.

This release also implements some other Zcash consensus rules, to check that
Zebra's [validation architecture](#architecture) supports future work on a
full validating node:
- block and transaction structure
- checkpoint-based verification up to and including Canopy activation
- transaction validation (incomplete)
- transaction cryptography (incomplete)
- transaction scripts (incomplete)
- batch verification (incomplete)

## Known Issues

There are a few bugs in Zebra that we're still working on fixing:
- [In rare cases, Zebra panics on shutdown #1678](https://github.com/ZcashFoundation/zebra/issues/1678)
  - For examples, see [#2055](https://github.com/ZcashFoundation/zebra/issues/2055) and [#2209](https://github.com/ZcashFoundation/zebra/issues/2209)
  - These panics can be ignored, unless they happen frequently
- [Interrupt handler does not work when a blocking task is running #1351](https://github.com/ZcashFoundation/zebra/issues/1351)
  - Zebra should eventually exit once the task finishes. Or you can forcibly terminate the process.
- [Duplicate block errors #1372](https://github.com/ZcashFoundation/zebra/issues/1372)
  - These errors can be ignored, unless they happen frequently

Zebra's state commits changes using database transactions.
If you forcibly terminate it, or it panics, any incomplete changes will be rolled back the next time it starts.

## Future Work

In 2021, we intend to finish validation, add RPC support, and add wallet integration.
This phased approach allows us to test Zebra's independent implementation of the
consensus rules, before asking users to entrust it with their funds.

Features:
- full consensus rule validation
- transaction mempool
- wallet functionality
- RPC functionality

Performance and Reliability:
- reliable syncing on Testnet
- reliable syncing under poor network conditions
- batch verification
- performance tuning

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
