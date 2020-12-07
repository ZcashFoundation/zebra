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

Our first alpha release is planned for December 8th, 2020.

The goals of this release are to:
- participate in the Zcash network,
- replicate the Zcash chain state, and
- implement the Zcash proof of work consensus rules.

### Getting Started

Run `cargo ...` **TODO**

If you're interested in testing out `zebrad` please feel free, but keep in mind
that there is a lot of key functionality still missing.

### System Requirements

**TBD**

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
- transaction cryptography (incomplete)
- transaction scripts (incomplete)
- batch verification (incomplete)

### Future Work

In 2021, we intend to add RPC support and wallet integration.  This phased
approach allows us to test Zebra's independent implementation of the consensus
rules, before asking users to entrust it with their funds.

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

### Known Issues

There are a few bugs in the Zebra alpha release that we're still working on
fixing:
- [Occasional panics in the `tokio` time wheel implementation #1452](https://github.com/ZcashFoundation/zebra/issues/1452)
- [Peer connections sometimes fail permanently #1435](https://github.com/ZcashFoundation/zebra/issues/1435)
  - these permanent failures can happen after a network disconnection, sleep, or individual peer disconnections

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

## License

Zebra is distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
