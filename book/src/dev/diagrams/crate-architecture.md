# Crate Architecture

This diagram shows the dependency relationships between Zebra's crates and the data flow through the system.

## Crate Dependency Graph

```mermaid
flowchart TB
    subgraph Application["Application Layer"]
        zebrad["zebrad<br/><i>Main binary</i>"]
    end

    subgraph Services["Service Layer"]
        rpc["zebra-rpc<br/><i>JSON-RPC server</i>"]
        network["zebra-network<br/><i>P2P networking</i>"]
    end

    subgraph Verification["Verification Layer"]
        consensus["zebra-consensus<br/><i>Semantic validation</i>"]
        state["zebra-state<br/><i>Contextual validation + storage</i>"]
    end

    subgraph Middleware["Tower Middleware"]
        batch["tower-batch-control<br/><i>Batch processing</i>"]
        fallback["tower-fallback<br/><i>Fallback verification</i>"]
    end

    subgraph Foundation["Foundation Layer"]
        chain["zebra-chain<br/><i>Core data structures</i>"]
        script["zebra-script<br/><i>Script verification</i>"]
        services["zebra-node-services<br/><i>Service traits</i>"]
    end

    subgraph External["External Dependencies"]
        rocksdb["RocksDB<br/><i>State storage</i>"]
        zcash_script["zcash_script<br/><i>C++ script lib</i>"]
        tower["Tower<br/><i>Service framework</i>"]
    end

    %% Application dependencies
    zebrad --> rpc
    zebrad --> network
    zebrad --> state
    zebrad --> consensus

    %% Service dependencies
    rpc --> state
    rpc --> consensus
    rpc --> chain
    rpc --> services
    rpc --> network
    rpc --> script

    network --> chain

    %% Verification dependencies
    consensus --> state
    consensus --> chain
    consensus --> script
    consensus --> batch
    consensus --> fallback
    consensus --> services

    state --> chain
    state --> services
    state --> rocksdb

    %% Foundation dependencies
    script --> chain
    script --> zcash_script

    services --> chain
    services --> tower

    %% Middleware dependencies
    batch --> tower
    fallback --> tower

    %% Styling
    classDef appLayer fill:#e1f5fe,stroke:#01579b
    classDef serviceLayer fill:#f3e5f5,stroke:#4a148c
    classDef verifyLayer fill:#fff3e0,stroke:#e65100
    classDef foundLayer fill:#e8f5e9,stroke:#1b5e20
    classDef external fill:#fce4ec,stroke:#880e4f
    classDef middleware fill:#fff8e1,stroke:#f57f17

    class zebrad appLayer
    class rpc,network serviceLayer
    class consensus,state verifyLayer
    class chain,script,services foundLayer
    class rocksdb,zcash_script,tower external
    class batch,fallback middleware
```

## Data Flow

```mermaid
sequenceDiagram
    participant P as Peer Network
    participant N as zebra-network
    participant S as Syncer (zebrad)
    participant C as zebra-consensus
    participant St as zebra-state
    participant R as RocksDB

    P->>N: Block announcement
    N->>S: Inbound request
    S->>N: Request block
    N->>P: GetData
    P->>N: Block data
    N->>S: Block response

    S->>C: Verify block (semantic)
    C->>C: Check signatures/proofs
    C-->>S: Verification result

    S->>St: Commit block (contextual)
    St->>St: Check UTXOs/nullifiers
    St->>R: Write to database
    R-->>St: Confirmation
    St-->>S: Block committed
```

## Verification Pipeline

Zebra uses a three-stage verification pipeline for maximum parallelism:

| Stage          | Crate             | What It Checks                       | Parallelizable           |
| -------------- | ----------------- | ------------------------------------ | ------------------------ |
| **Structural** | `zebra-chain`     | Data format, type constraints        | Yes (at parse time)      |
| **Semantic**   | `zebra-consensus` | Signatures, proofs, scripts          | Yes (batch verification) |
| **Contextual** | `zebra-state`     | UTXO set, nullifier set, chain rules | Partially (per-block)    |

## Crate Responsibilities

### Core Crates

| Crate             | Lines of Code | Primary Responsibility                                 |
| ----------------- | ------------- | ------------------------------------------------------ |
| `zebra-chain`     | ~40K          | Block, Transaction, Address definitions; serialization |
| `zebra-consensus` | ~13K          | Signature/proof verification; checkpoint sync          |
| `zebra-state`     | ~35K          | RocksDB storage; UTXO/nullifier tracking               |
| `zebra-network`   | ~26K          | Peer connections; protocol translation                 |
| `zebra-rpc`       | ~17K          | JSON-RPC methods; mining block templates               |
| `zebrad`          | ~20K          | Service orchestration; configuration                   |

### Supporting Crates

| Crate                 | Purpose                                                  |
| --------------------- | -------------------------------------------------------- |
| `zebra-script`        | Wraps `zcash_script` C++ library for transparent scripts |
| `zebra-node-services` | Shared trait definitions (Mempool, BoxError)             |
| `zebra-test`          | Test vectors, mock services, test utilities              |
| `zebra-utils`         | CLI tools: checkpoint generator, OpenAPI generator       |
| `tower-batch-control` | Automatic batching for verification requests             |
| `tower-fallback`      | Falls back to single verification on batch failure       |

## See Also

- [Service Dependencies Diagram](./service-dependencies.svg) - Runtime service interactions
- [Network Architecture](./zebra-network.md) - Detailed network design
- [Mempool Architecture](./mempool-architecture.md) - Transaction pool design
- [Design Overview](../overview.md) - High-level architecture description
