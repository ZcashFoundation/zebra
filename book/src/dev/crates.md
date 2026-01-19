# Zebra Crates Reference

This page provides a quick reference for all crates in the Zebra workspace.
For a high-level design overview, see the [Design Overview](overview.md).

## Quick Reference Table

| Crate                                         | Purpose             | Key Types                             | Entry Point       |
| --------------------------------------------- | ------------------- | ------------------------------------- | ----------------- |
| [`zebrad`](#zebrad)                           | Main application    | `ZebradCmd`, `Config`                 | `main()`          |
| [`zebra-chain`](#zebra-chain)                 | Data structures     | `Block`, `Transaction`, `Address`     | N/A (library)     |
| [`zebra-consensus`](#zebra-consensus)         | Verification        | `SemanticBlockVerifier`, `CheckpointVerifier` | `init()`          |
| [`zebra-state`](#zebra-state)                 | Storage             | `ReadStateService`, `Request`, `Response` | `init()`          |
| [`zebra-network`](#zebra-network)             | P2P networking      | `PeerSet`, `Request`, `Response`      | `init()`          |
| [`zebra-rpc`](#zebra-rpc)                     | JSON-RPC server     | `RpcServer`, methods                  | `init()`          |
| [`zebra-node-services`](#zebra-node-services) | Service traits      | `Mempool`, `BoxError`                 | N/A (traits)      |
| [`zebra-script`](#zebra-script)               | Script verification | `is_valid()`                          | Direct function   |
| [`zebra-test`](#zebra-test)                   | Test utilities      | `init()`, mock services               | `init()`          |
| [`zebra-utils`](#zebra-utils)                 | CLI tools           | Various binaries                      | CLI               |
| [`tower-batch-control`](#tower-batch-control) | Batch middleware    | `Batch`, `BatchControl`               | `Batch::new()`    |
| [`tower-fallback`](#tower-fallback)           | Fallback middleware | `Fallback`                            | `Fallback::new()` |

---

## Detailed Crate Information

### zebrad

**Purpose**: Main Zebra node application. Orchestrates all services, handles configuration, and manages lifecycle.

**Location**: `/zebrad`

**Binary**: `zebrad`

**Key Types**:

- `ZebradCmd` - Command-line interface
- `Config` - Application configuration
- `Application` - Abscissa application state

**Key Modules**:

- `commands/` - CLI commands (start, generate, etc.)
- `components/` - Service components (sync, mempool, inbound)

**Docs**: [docs.rs/zebrad](https://docs.rs/zebrad)

---

### zebra-chain

**Purpose**: Core Zcash data structures with consensus-critical serialization. Foundation crate with no internal dependencies.

**Location**: `/zebra-chain`

**Key Types**:

- `Block`, `BlockHeader` - Block structures
- `Transaction` - All transaction versions (V1-V5)
- `Address` - Transparent and shielded addresses
- `Amount<C>` - Type-safe amounts with constraints
- `Network` - Mainnet/Testnet configuration

**Key Traits**:

- `ZcashSerialize` / `ZcashDeserialize` - Consensus serialization

**Key Modules**:

- `block/` - Block definitions
- `transaction/` - Transaction types
- `sapling/`, `orchard/` - Shielded protocol types
- `transparent/` - Transparent (Bitcoin-style) types

**Docs**: [docs.rs/zebra_chain](https://docs.rs/zebra_chain)

---

### zebra-consensus

**Purpose**: Semantic verification of blocks and transactions. Validates signatures, proofs, and scripts independently of chain state.

**Location**: `/zebra-consensus`

**Key Types**:

- `SemanticBlockVerifier` - Verifies individual blocks semantically
- `CheckpointVerifier` - Fast-syncs using checkpoints
- `transaction::Verifier` - Verifies transactions semantically

**Key Enums**:

- `BlockError` - Block verification errors
- `TransactionError` - Transaction verification errors

**Entry Point**:

```rust
let verifier = zebra_consensus::init(config, network, state_service).await;
```

**Docs**: [docs.rs/zebra_consensus](https://docs.rs/zebra_consensus)

---

### zebra-state

**Purpose**: Contextual verification and persistent storage. Manages the chain state in RocksDB, tracks UTXOs and nullifiers.

**Location**: `/zebra-state`

**Key Types**:

- `ReadStateService` - Read-only state access for RPC and other components
- `Request` - State queries (blocks, transactions, UTXOs)
- `Response` - State responses
- `LatestChainTip` - Chain tip watcher
- `ChainTipChange` - Chain tip change notifications

**Entry Point**:

```rust
let (state_service, read_state, latest_chain_tip, chain_tip_change) =
    zebra_state::init(config, network, max_checkpoint_height, checkpoint_verify_concurrency_limit).await;
// state_service: BoxService<Request, Response, BoxError>
// read_state: ReadStateService
```

**Docs**: [docs.rs/zebra_state](https://docs.rs/zebra_state)

---

### zebra-network

**Purpose**: Zcash P2P networking. Manages peer connections, translates protocol messages, and provides connection pooling.

**Location**: `/zebra-network`

**Key Types**:

- `PeerSet` - Connection pool service
- `Request` - Network requests (GetBlocks, GetTx, etc.)
- `Response` - Network responses
- `Config` - Network configuration

**Entry Point**:

```rust
let (peer_set, address_book) = zebra_network::init(config, inbound_service).await;
```

**Key Features**:

- Automatic peer discovery
- Backpressure-driven connection management
- `connect_isolated()` for Tor-safe connections

**Docs**: [docs.rs/zebra_network](https://docs.rs/zebra_network)

---

### zebra-rpc

**Purpose**: JSON-RPC server for wallet and mining integration. Implements zcashd-compatible RPC methods.

**Location**: `/zebra-rpc`

**Key Types**:

- `RpcServer` - JSON-RPC server
- RPC method implementations

**Key RPC Methods**:

- `getblock`, `getblockchaininfo` - Block queries
- `getrawtransaction`, `sendrawtransaction` - Transaction handling
- `getblocktemplate` - Mining support
- `z_*` methods - Shielded operations

**Entry Point**:

```rust
let rpc_server = zebra_rpc::init(config, ...).await;
```

**Docs**: [docs.rs/zebra_rpc](https://docs.rs/zebra_rpc)

---

### zebra-node-services

**Purpose**: Shared service trait definitions and types used across crates.

**Location**: `/zebra-node-services`

**Key Types**:

- `BoxError` - Standard boxed error type
- Mempool service traits
- RPC client traits

**Usage**:

```rust
use zebra_node_services::BoxError;

type Result<T> = std::result::Result<T, BoxError>;
```

**Docs**: [docs.rs/zebra_node_services](https://docs.rs/zebra_node_services)

---

### zebra-script

**Purpose**: Transparent script verification. Wraps the `zcash_script` C++ library.

**Location**: `/zebra-script`

**Key Function**:

```rust
pub fn is_valid(
    transaction: &Transaction,
    branch_id: ConsensusBranchId,
    input_index: usize,
    amount: Amount,
    script_pub_key: &Script,
) -> Result<(), Error>
```

**Docs**: [docs.rs/zebra_script](https://docs.rs/zebra_script)

---

### zebra-test

**Purpose**: Test utilities, mock services, and test vectors for the Zebra test suite.

**Location**: `/zebra-test`

**Key Functions**:

- `init()` - Initialize test environment (tracing, etc.)
- `vectors::*` - Test vectors from Zcash specs

**Usage**:

```rust
#[test]
fn my_test() {
    let _init_guard = zebra_test::init();
    // test code
}
```

**Docs**: [docs.rs/zebra_test](https://docs.rs/zebra_test)

---

### zebra-utils

**Purpose**: Developer CLI tools for Zebra maintenance and operations.

**Location**: `/zebra-utils`

**Binaries**:

- `zebra-checkpoints` - Generate blockchain checkpoints
- `openapi-generator` - Generate OpenAPI specification
- `block-template-to-proposal` - Convert mining templates
- `search-issue-refs` - Search for issue references in code

**Usage**:

```bash
cargo run --bin zebra-checkpoints -- --help
```

---

### tower-batch-control

**Purpose**: Tower middleware for automatic batch processing. Collects multiple verification requests and processes them together for efficiency.

**Location**: `/tower-batch-control`

**Key Types**:

- `Batch<S>` - Batching wrapper for services
- `BatchControl<R>` - Control messages

**Usage**:

```rust
let batched_service = Batch::new(verifier, max_items, max_latency);
```

**Docs**: [docs.rs/tower_batch_control](https://docs.rs/tower_batch_control)

---

### tower-fallback

**Purpose**: Tower middleware for fallback verification. If batch verification fails, falls back to individual item verification.

**Location**: `/tower-fallback`

**Key Types**:

- `Fallback<S, F>` - Fallback wrapper

**Usage**:

```rust
let service = Fallback::new(primary_service, fallback_service);
```

**Docs**: [docs.rs/tower_fallback](https://docs.rs/tower_fallback)

---

## Dependency Graph

For a visual representation of how these crates depend on each other, see the [Crate Architecture Diagram](diagrams/crate-architecture.md).

## Adding a New Crate

When adding a new crate to the workspace:

1. Create the crate directory with `cargo new --lib crate-name`
2. Add it to the workspace in root `Cargo.toml`
3. Add a `README.md` following the format above
4. Add documentation to `lib.rs` with `//!` comments
5. Update this reference page

---

## Crate Ownership on crates.io

The Zebra project publishes crates to [crates.io](https://crates.io).
Zcash Foundation crates are controlled by the [`ZcashFoundation/owners`](https://github.com/orgs/ZcashFoundation/teams/owners) GitHub team.

The latest list of Zebra and FROST crates is [available on crates.io](https://crates.io/teams/github:zcashfoundation:owners).

### Crates Published from This Repository

- All crates starting with `zebra` (including `zebrad` and the `zebra` placeholder)
- All crates starting with `tower`

### Related ZF Crates (Separate Repositories)

- `zcash_script`
- `ed25519-zebra`

### Crates Shared with ECC

- `reddsa`
- `redjubjub`

### Logging in to crates.io

To publish a crate or change owners, [log in to crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html#before-your-first-publish) using `cargo login`.

When creating a token:

- Set an expiry date
- Limit permissions to the specific task
- [Revoke the token](https://crates.io/me) after use

### Publishing New Placeholder Crates

We publish placeholder crates as soon as we have a good name. From the `main` branch:

```sh
cargo new new-crate-name
cd new-crate-name
cargo release publish --verbose --package new-crate-name --execute
cargo owner --add github:zcashfoundation:owners
```

### Ownership Requirements

Zcash Foundation crates should have:

- At least 2 individual owners (typically project engineers)
- A group owner containing everyone who can publish

When an individual owner leaves the foundation, [replace them with another individual owner](https://doc.rust-lang.org/cargo/reference/publishing.html#cargo-owner).

New owners must accept invitations at [crates.io/me](https://crates.io/me).
