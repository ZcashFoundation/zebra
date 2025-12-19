# Design Overview

This document sketches the design for Zebra.

## Desiderata

The following are general desiderata for Zebra:

* [George's list..]

* As much as reasonably possible, it and its dependencies should be
  implemented in Rust.  While it may not make sense to require this in
  every case (for instance, it probably doesn't make sense to rewrite
  libsecp256k1 in Rust, instead of using the same upstream library as
  Bitcoin), we should generally aim for it.

* As much as reasonably possible, Zebra should minimize trust in
  required dependencies.  Note that "minimize number of dependencies"
  is usually a proxy for this desideratum, but is not exactly the same:
  for instance, a collection of crates like the tokio crates are all
  developed together and have one trust boundary.

* Zebra should be well-factored internally into a collection of
  component libraries which can be used by other applications to
  perform Zcash-related tasks.  Implementation details of each
  component should not leak into all other components.

* Zebra should checkpoint on Canopy activation and drop all
  Sprout-related functionality not required post-Canopy.

## Non-Goals

* Zebra keeps a copy of the chain state, so it isn't intended for
  lightweight applications like light wallets. Those applications
  should use a light client protocol.

## Notable Blog Posts
- [A New Network Stack For Zcash](https://www.zfnd.org/blog/a-new-network-stack-for-zcash)
- [Composable Futures-based Batch Verification](https://www.zfnd.org/blog/futures-batch-verification)
- [Decoding Bitcoin Messages with Tokio Codecs](https://www.zfnd.org/blog/decoding-bitcoin-messages-with-tokio-codecs)

## Service Dependencies

<div id="service-dep-diagram">
{{#include diagrams/service-dependencies.svg}}
</div>

<!-- 
Service dependencies diagram source:

digraph services {
    transaction_verifier -> state
    mempool -> state
    inbound -> state
    rpc_server -> state
    mempool -> transaction_verifier
    block_verifier_router -> checkpoint_verifier
    inbound -> mempool
    rpc_server -> mempool
    inbound -> block_verifier_router
    syncer -> block_verifier_router
    rpc_server -> block_verifier_router [style=dotted]
    syncer -> peer_set
    mempool -> peer_set
    block_verifier -> state
    checkpoint_verifier -> state
    block_verifier -> transaction_verifier
    block_verifier_router -> block_verifier
    rpc_server -> inbound [style=invis] // for layout of the diagram
}

Render here: https://dreampuf.github.io/GraphvizOnline
 -->

The dotted lines are for the `getblocktemplate` RPC.

## Architecture

Unlike `zcashd`, which originated as a Bitcoin Core fork and inherited its
monolithic architecture, Zebra has a modular, library-first design, with the
intent that each component can be independently reused outside of the `zebrad`
full node. For instance, the `zebra-network` crate containing the network stack
can also be used to implement anonymous transaction relay, network crawlers, or
other functionality, without requiring a full node.

At a high level, the fullnode functionality required by `zebrad` is factored
into several components:

- [`zebra-chain`](https://docs.rs/zebra_chain), providing
  definitions of core data structures for Zcash, such as blocks, transactions,
  addresses, etc., and related functionality.  It also contains the
  implementation of the consensus-critical serialization formats used in Zcash.
  The data structures in `zebra-chain` are defined to enforce
  [*structural validity*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages)
  by making invalid states unrepresentable. For instance, the
  `Transaction` enum has variants for each transaction version, and it's
  impossible to construct a transaction with, e.g., spend or output
  descriptions but no binding signature, or, e.g., a version 2 (Sprout)
  transaction with Sapling proofs. Currently, `zebra-chain` is oriented
  towards verifying transactions, but will be extended to support creating them
  in the future.

- [`zebra-network`](https://docs.rs/zebra_network),
  providing an asynchronous, multithreaded implementation of the Zcash network
  protocol inherited from Bitcoin. In contrast to `zcashd`, each peer
  connection has a separate state machine, and the crate translates the
  external network protocol into a stateless, request/response-oriented
  protocol for internal use. The crate provides two interfaces:
  - an auto-managed connection pool that load-balances local node requests
    over available peers, and sends peer requests to a local inbound service,
    and
  - a `connect_isolated` method that produces a peer connection completely
    isolated from all other node state.  This can be used, for instance, to
    safely relay data over Tor, without revealing distinguishing information.

- [`zebra-script`](https://docs.rs/zebra_script) provides
  script validation. Currently, this is implemented by linking to the C++
  script verification code from `zcashd`, but in the future we may implement a
  pure-Rust script implementation.

- [`zebra-consensus`](https://docs.rs/zebra_consensus)
  performs [*semantic validation*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages)
  of blocks and transactions: all consensus
  rules that can be checked independently of the chain state, such as
  verification of signatures, proofs, and scripts. Internally, the library
  uses [`tower-batch-control`](https://docs.rs/tower_batch_control) to
  perform automatic, transparent batch processing of contemporaneous
  verification requests.

- [`zebra-state`](https://docs.rs/zebra_state) is
  responsible for storing, updating, and querying the chain state. The state
  service is responsible for [*contextual verification*](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#verification-stages):
  all consensus rules
  that check whether a new block is a valid extension of an existing chain,
  such as updating the nullifier set or checking that transaction inputs remain
  unspent.

- [`zebrad`](https://docs.rs/zebrad) contains the full
  node, which connects these components together and implements logic to handle
  inbound requests from peers and the chain sync process.

All of these components can be reused as independent libraries, and all
communication between stateful components is handled internally by
[internal asynchronous RPC abstraction](https://docs.rs/tower/)
("microservices in one process").

### `zebra-chain`

#### Internal Dependencies

None: these are the core data structure definitions.

#### Responsible for

- definitions of commonly used data structures, e.g.,
  - `Block`,
  - `Transaction`,
  - `Address`,
  - `KeyPair`...
- parsing bytes into these data structures

- definitions of core traits, e.g.,
  - `ZcashSerialize` and `ZcashDeserialize`, which perform
    consensus-critical serialization logic.

#### Exported types

- [...]

### `zebra-network`

#### Internal Dependencies

- `zebra-chain`

#### Responsible for

- definition of a well structured, internal request/response protocol
- provides an abstraction for "this node" and "the network" using the
  internal protocol
- dynamic, backpressure-driven peer set management
- per-peer state machine that translates the internal protocol to the
  Bitcoin/Zcash protocol
- tokio codec for Bitcoin/Zcash message encoding.

#### Exported types

- `Request`, an enum representing all possible requests in the internal protocol;
- `Response`, an enum representing all possible responses in the internal protocol;
- `AddressBook`, a data structure for storing peer addresses;
- `Config`, a configuration object for all networking-related parameters;
- `init<S: Service>(Config, S) -> (impl Service,
  Arc<Mutex<AddressBook>>)`, the main entry-point.

The `init` entrypoint constructs a dynamically-sized pool of peers
sending inbound requests to the provided `S: tower::Service`
representing "this node", and returns a `Service` that can be used to
send requests to "the network", together with an `AddressBook` updated
with liveness information from the peer pool.  The `AddressBook` can
be used to respond to inbound requests for peers.

All peerset management (finding new peers, creating new outbound
connections, etc) is completely encapsulated, as is responsibility for
routing outbound requests to appropriate peers.

### `zebra-state`

#### Internal Dependencies

- `zebra-chain` for data structure definitions.

#### Responsible for

- block storage API
  - operates on parsed block structs
    - these structs can be converted from and into raw bytes
  - primarily aimed at network replication, not at processing
  - can be used to rebuild the database below
- maintaining a database of tx, address, etc data
  - this database can be blown away and rebuilt from the blocks, which
    are otherwise unused.
  - threadsafe, typed lookup API that completely encapsulates the
    database logic
  - handles stuff like "transactions are reference counted by outputs"
    etc.
- providing `tower::Service` interfaces for all of the above to
  support backpressure.

#### Exported types

- `Request`, an enum representing all possible requests in the internal protocol;
  - blocks can be accessed via their chain height or hash
  - confirmed transactions can be accessed via their block, or directly via their hash
- `Response`, an enum representing all possible responses in the internal protocol;
- `init() -> impl Service`, the main entry-point.

The `init` entrypoint returns a `Service` that can be used to
send requests for the chain state.

All state management (adding blocks, getting blocks by index or hash) is completely
encapsulated.

### `zebra-script`

#### Internal Dependencies

#### Responsible for

- the minimal Bitcoin script implementation required for Zcash
- script parsing
- context-free script validation

#### Notes

This can wrap an existing script implementation at the beginning.

If this existed in a "good" way, we could use it to implement tooling
for Zcash script inspection, debugging, etc.

#### Questions

- How does this interact with NU4 script changes?

#### Exported types

- [...]

### `zebra-consensus`

#### Internal Dependencies

- `zebra-chain` for data structures and parsing.
- `zebra-state` to read and update the state database.
- `zebra-script` for script parsing and validation.

#### Responsible for

- consensus-specific parameters (network magics, genesis block, pow
  parameters, etc) that determine the network consensus
- consensus logic to decide which block is the current block
- block and transaction verification
  - context-free validation, e.g., signature, proof verification, etc.
  - context-dependent validation, e.g., determining whether a
    transaction is accepted in a particular chain state context.
  - verifying mempool (unconfirmed) transactions
- block checkpoints
  - mandatory checkpoints (genesis block, canopy activation)
  - optional regular checkpoints (every Nth block)
- modifying the chain state
  - adding new blocks to `ZebraState`, including chain reorganisation
  - adding new transactions to `ZebraMempoolState`
- storing the transaction mempool state
  - mempool transactions can be accessed via their hash
- providing `tower::Service` interfaces for all of the above to
  support backpressure and batch validation.

#### Exported types

- `block::init() -> impl Service`, the main entry-point for block
  verification.
- `ZebraMempoolState`
  - all state management (adding transactions, getting transactions
    by hash) is completely encapsulated.
- `mempool::init() -> impl Service`, the main entry-point for
    mempool transaction verification.

The `init` entrypoints return `Service`s that can be used to
verify blocks or transactions, and add them to the relevant state.

### `zebra-rpc`

#### Internal Dependencies

- `zebra-chain` for data structure definitions
- `zebra-node-services` for shared request type definitions
- `zebra-utils` for developer and power user tools

#### Responsible for

- rpc interface

#### Exported types

- [...]

### `zebra-client`

#### Internal Dependencies

- `zebra-chain` for structure definitions
- `zebra-state` for transaction queries and client/wallet state storage
- `zebra-script` possibly? for constructing transactions

#### Responsible for

- implementation of some event a user might trigger
- would be used to implement a full wallet
- create transactions, monitors shielded wallet state, etc.

#### Notes

Communication between the client code and the rest of the node should be done
by a tower service interface. Since the `Service` trait can abstract from a
function call to RPC, this means that it will be possible for us to isolate
all client code to a subprocess.

#### Exported types

- [...]

### `zebrad`

Abscissa-based application which loads configs, all application components,
and connects them to each other.

#### Responsible for

- actually running the server
- connecting functionality in dependencies

#### Internal Dependencies

- `zebra-chain`
- `zebra-network`
- `zebra-state`
- `zebra-consensus`
- `zebra-client`
- `zebra-rpc`

### Unassigned functionality

Responsibility for this functionality needs to be assigned to one of
the modules above (subject to discussion):

- [ ... add to this list ... ]
