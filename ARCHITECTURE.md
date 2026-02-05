# Zebra Architecture

Zebra is an independent, consensus-compatible Zcash full node implementation in Rust. Unlike `zcashd` (a Bitcoin Core fork), Zebra uses a modular, library-first design where each component can be independently reused.

## Crate Overview

| Crate | Purpose |
|-------|---------|
| [`zebrad`](https://docs.rs/zebrad) | Main application binary - orchestrates all services |
| [`zebra-chain`](https://docs.rs/zebra-chain) | Core data structures (blocks, transactions, addresses) |
| [`zebra-consensus`](https://docs.rs/zebra-consensus) | Semantic verification (signatures, proofs, scripts) |
| [`zebra-state`](https://docs.rs/zebra-state) | Contextual verification and RocksDB storage |
| [`zebra-network`](https://docs.rs/zebra-network) | P2P networking with peer management |
| [`zebra-rpc`](https://docs.rs/zebra-rpc) | JSON-RPC server for wallet/miner integration |

## Design Principles

- **Modular Architecture**: Independent crates that can be used separately
- **Type-Safe Consensus**: Invalid states are unrepresentable in Rust's type system
- **Async-First**: Built on [Tower](https://docs.rs/tower/) services for composability
- **Three-Level Verification**: Structural (types) → Semantic (crypto) → Contextual (chain state)

## Documentation

For detailed architecture documentation, see the [Zebra Book](https://zebra.zfnd.org/):

- **[Design Overview](https://zebra.zfnd.org/dev/overview.html)** - High-level architecture and component responsibilities
- **[Crate Architecture](https://zebra.zfnd.org/dev/diagrams/crate-architecture.html)** - Visual dependency graphs and data flow
- **[Crate Reference](https://zebra.zfnd.org/dev/crates.html)** - Detailed documentation for each crate
- **[RFCs](https://zebra.zfnd.org/dev/rfcs.html)** - Design decisions and rationale
- **[API Documentation](https://docs.rs/zebrad)** - Rust API docs on docs.rs
