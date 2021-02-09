![Zebra logotype](https://www.zfnd.org/images/zebra-logotype.png)

---

[![](https://github.com/ZcashFoundation/zebra/workflows/CI/badge.svg?branch=main)](https://github.com/ZcashFoundation/zebra/actions?query=workflow%3ACI+branch%3Amain)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

## About

[Zebra](https://zebra.zfnd.org/) is the Zcash Foundation's independent,
consensus-compatible implementation of the Zcash protocol, currently under
development.  Please [join us on Discord](https://discord.gg/na6QZNd) if you'd
like to find out more or get involved!

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

### Getting Started

Building `zebrad` requires [Rust](https://www.rust-lang.org/tools/install),
[libclang](https://clang.llvm.org/get_started.html), and a C++ compiler.

#### Detailed Build and Run Instructions

1. Install [`cargo` and `rustc`](https://www.rust-lang.org/tools/install). 
     - Using `rustup` installs the stable Rust toolchain, which `zebrad` targets.
2. Install Zebra's build dependencies:
     - **libclang:** the `libclang`, `libclang-dev`, `llvm`, or `llvm-dev` packages, depending on your package manager
     - **clang** or another C++ compiler: `g++`, `Xcode`, or `MSVC`
3. Run `cargo install --locked --git https://github.com/ZcashFoundation/zebra --tag v1.0.0-alpha.2 zebrad`
4. Run `zebrad start`

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

### Network Usage

`zebrad`'s typical network usage is:
- initial sync: 30 GB download
- ongoing updates: 10-50 MB upload and download per day, depending on peer requests

The major constraint we've found on `zebrad` performance is the network weather,
especially the ability to make good connections to other Zcash network peers.

### Current Features

Network:
- synchronize the chain from peers
- download gossipped blocks from peers
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
- checkpoint-based verification up to Sapling
- transaction validation (incomplete)
- transaction cryptography (incomplete)
- transaction scripts (incomplete)
- batch verification (incomplete)

### Dependencies

Zebra primarily depends on pure Rust crates, and some Rust/C++ crates:
- [rocksdb](https://crates.io/crates/rocksdb)
- [zcash_script](https://crates.io/crates/zcash_script)

### Known Issues

There are a few bugs in Zebra that we're still working on fixing:
- [Peer connections sometimes fail permanently #1435](https://github.com/ZcashFoundation/zebra/issues/1435)
  - these permanent failures can happen after a network disconnection, sleep, or individual peer disconnections
  - workaround: use `Control-C` to exit `zebrad`, and then restart `zebrad`
- [Duplicate block errors #1372](https://github.com/ZcashFoundation/zebra/issues/1372)
  - these errors can be ignored, unless they happen frequently

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

## Architecture

Unlike `zcashd`, which originated as a Bitcoin Core fork and inherited its
monolithic architecture, Zebra has a modular, library-first design, with the
intent that each component can be independently reused outside of the `zebrad`
full node.  For instance, the `zebra-network` crate containing the network stack
can also be used to implement anonymous transaction relay, network crawlers, or
other functionality, without requiring a full node.

At a high level, the fullnode functionality required by `zebrad` is factored
into several components:

- [`zebra-chain`](https://doc.zebra.zfnd.org/zebra_chain/index.html), providing
  definitions of core data structures for Zcash, such as blocks, transactions,
  addresses, etc., and related functionality.  It also contains the
  implementation of the consensus-critical serialization formats used in Zcash.
  The data structures in `zebra-chain` are defined to enforce
  [*structural validity*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages)
  by making invalid states unrepresentable.  For instance, the
  `Transaction` enum has variants for each transaction version, and it's
  impossible to construct a transaction with, e.g., spend or output
  descriptions but no binding signature, or, e.g., a version 2 (Sprout)
  transaction with Sapling proofs.  Currently, `zebra-chain` is oriented
  towards verifying transactions, but will be extended to support creating them
  in the future.

- [`zebra-network`](https://doc.zebra.zfnd.org/zebra_network/index.html),
  providing an asynchronous, multithreaded implementation of the Zcash network
  protocol inherited from Bitcoin.  In contrast to `zcashd`, each peer
  connection has a separate state machine, and the crate translates the
  external network protocol into a stateless, request/response-oriented
  protocol for internal use.  The crate provides two interfaces:
  - an auto-managed connection pool that load-balances local node requests
    over available peers, and sends peer requests to a local inbound service,
    and
  - a `connect_isolated` method that produces a peer connection completely
    isolated from all other node state.  This can be used, for instance, to
    safely relay data over Tor, without revealing distinguishing information.

- [`zebra-script`](https://doc.zebra.zfnd.org/zebra_script/index.html) provides
  script validation.  Currently, this is implemented by linking to the C++
  script verification code from `zcashd`, but in the future we may implement a
  pure-Rust script implementation.

- [`zebra-consensus`](https://doc.zebra.zfnd.org/zebra_consensus/index.html)
  performs [*semantic validation*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages)
  of blocks and transactions: all consensus
  rules that can be checked independently of the chain state, such as
  verification of signatures, proofs, and scripts.  Internally, the library
  uses [`tower-batch`](https://doc.zebra.zfnd.org/tower_batch/index.html) to
  perform automatic, transparent batch processing of contemporaneous
  verification requests.

- [`zebra-state`](https://doc.zebra.zfnd.org/zebra_state/index.html) is
  responsible for storing, updating, and querying the chain state.  The state
  service is responsible for [*contextual verification*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages):
  all consensus rules
  that check whether a new block is a valid extension of an existing chain,
  such as updating the nullifier set or checking that transaction inputs remain
  unspent.

- [`zebrad`](https://doc.zebra.zfnd.org/zebrad/index.html) contains the full
  node, which connects these components together and implements logic to handle
  inbound requests from peers and the chain sync process.

- `zebra-rpc` and `zebra-client` will eventually contain the RPC and wallet
  functionality, but as mentioned above, our goal is to implement replication
  of chain state first before asking users to entrust Zebra with their funds.

All of these components can be reused as independent libraries, and all
communication between stateful components is handled internally by
[internal asynchronous RPC abstraction](https://docs.rs/tower/)
("microservices in one process").

## Security

Zebra has a [responsible disclosure policy](https://github.com/ZcashFoundation/zebra/blob/main/responsible_disclosure.md), which we encourage security researchers to follow.

## License

Zebra is distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
