# Mempool Specification

The Zebra mempool handles unmined Zcash transactions: collecting them from peers, verifying them, storing them in memory, providing APIs for other components to access them, and gossiping transactions to peers. This document specifies the architecture, behavior, and interfaces of the mempool.

## Overview

The mempool is a fundamental component of the Zebra node, responsible for managing the lifecycle of unmined transactions. It provides an in-memory storage for valid transactions that haven't yet been included in a block, and offers interfaces for other components to interact with these transactions.

Key responsibilities of the mempool include:
- Accepting new transactions from the network
- Verifying transactions against a subset of consensus rules
- Storing verified transactions in memory
- Managing memory usage and transaction eviction
- Providing transaction queries to other components
- Gossiping transactions to peers

## Architecture

The mempool is comprised of several subcomponents:

1. **Mempool Service** (`Mempool`): The main service that handles requests from other components, manages the active state of the mempool, and coordinates the other subcomponents.

2. **Transaction Storage** (`Storage`): Manages the in-memory storage of verified transactions and rejected transactions, along with their rejection reasons.

3. **Transaction Downloads** (`Downloads`): Handles downloading and verifying transactions, coordinating with the network and verification services.

4. **Crawler** (`Crawler`): Periodically polls peers for new transactions to add to the mempool.

5. **Queue Checker** (`QueueChecker`): Regularly checks the transaction verification queue to process newly verified transactions.

6. **Transaction Gossip** (`gossip`): Broadcasts newly added transactions to peers.

7. **Pending Outputs** (`PendingOutputs`): Tracks requests for transaction outputs that haven't yet been seen.

For a visual representation of the architecture and transaction flow, see the [Mempool Architecture Diagram](diagrams/mempool-architecture.md).

## Activation

The mempool is activated when:
- The node is near the blockchain tip (determined by `SyncStatus`)
- OR when the current chain height reaches a configured debug height (`debug_enable_at_height`)

When activated, the mempool creates transaction download and verify services, initializes storage, and starts background tasks for crawling and queue checking.

## Configuration

The mempool has the following configurable parameters:

1. **Transaction Cost Limit** (`tx_cost_limit`): The maximum total serialized byte size of all transactions in the mempool, defaulting to 80,000,000 bytes as required by [ZIP-401](https://zips.z.cash/zip-0401).

2. **Eviction Memory Time** (`eviction_memory_time`): The maximum time to remember evicted transaction IDs in the rejection list, defaulting to 60 minutes.

3. **Debug Enable At Height** (`debug_enable_at_height`): An optional height at which to enable the mempool for debugging, regardless of sync status.

## State Management

The mempool maintains an `ActiveState` which can be either:
- `Disabled`: Mempool is not active
- `Enabled`: Mempool is active and contains:
  - `storage`: The Storage instance for transactions
  - `tx_downloads`: Transaction download and verification stream
  - `last_seen_tip_hash`: Hash of the last chain tip the mempool has seen

The mempool responds to chain tip changes:
- On new blocks: Updates verification context, removes mined transactions
- On reorgs: Clears tip-specific rejections, retries all transactions

## Transaction Processing Flow

1. **Transaction Arrival**:
   - From peer gossip (inv messages)
   - From direct submission (RPC)
   - From periodic peer polling (crawler)

2. **Transaction Download**:
   - Checks if transaction exists in mempool or rejection lists
   - Queues transaction for download if needed
   - Downloads transaction data from peers

3. **Transaction Verification**:
   - Checks transaction against consensus rules
   - Verifies transaction against the current chain state
   - Manages dependencies between transactions

4. **Transaction Storage**:
   - Stores verified transactions in memory
   - Tracks transaction dependencies
   - Enforces size limits and eviction policies

5. **Transaction Gossip**:
   - Broadcasts newly verified transactions to peers

## Transaction Rejection

Transactions can be rejected for multiple reasons, categorized into:

1. **Exact Tip Rejections** (`ExactTipRejectionError`):
   - Failures in consensus validation
   - Only applies to exactly matching transactions at the current tip

2. **Same Effects Tip Rejections** (`SameEffectsTipRejectionError`):
   - Spending conflicts with other mempool transactions
   - Missing outputs from mempool transactions
   - Applies to any transaction with the same effects at the current tip

3. **Same Effects Chain Rejections** (`SameEffectsChainRejectionError`):
   - Expired transactions
   - Duplicate spends already in the blockchain
   - Transactions already mined
   - Transactions evicted due to memory limits
   - Applies until a rollback or network upgrade

Rejection reasons are stored alongside rejected transaction IDs to prevent repeated verification of invalid transactions.

## Memory Management

The mempool employs several strategies for memory management:

1. **Transaction Cost Limit**: Enforces a maximum total size for all mempool transactions.

2. **Random Eviction**: When the mempool exceeds the cost limit, transactions are randomly evicted following the [ZIP-401](https://zips.z.cash/zip-0401) specification.

3. **Eviction Memory**: Remembers evicted transaction IDs for a configurable period to prevent re-verification.

4. **Rejection List Size Limit**: Caps rejection lists at 40,000 entries per [ZIP-401](https://zips.z.cash/zip-0401).

5. **Automatic Cleanup**: Removes expired transactions and rejections that are no longer relevant.

## Service Interface

The mempool exposes a service interface with the following request types:

1. **Query Requests**:
   - `TransactionIds`: Get all transaction IDs in the mempool
   - `TransactionsById`: Get transactions by their unmined IDs
   - `TransactionsByMinedId`: Get transactions by their mined hashes
   - `FullTransactions`: Get all verified transactions with fee information
   - `RejectedTransactionIds`: Query rejected transaction IDs
   - `TransactionWithDepsByMinedId`: Get a transaction and its dependencies

2. **Action Requests**:
   - `Queue`: Queue transactions or transaction IDs for download and verification
   - `CheckForVerifiedTransactions`: Check for newly verified transactions
   - `AwaitOutput`: Wait for a specific transparent output to become available

## Interaction with Other Components

The mempool interacts with several other Zebra components:

1. **Network Service**: For downloading transactions and gossiping to peers.

2. **State Service**: For checking transaction validity against the current chain state.

3. **Transaction Verifier**: For consensus validation of transactions.

4. **Chain Tip Service**: For tracking the current blockchain tip.

5. **RPC Services**: To provide transaction data for RPC methods.

## Implementation Constraints

1. **Correctness**:
   - All transactions in the mempool must be verified
   - Transactions must be re-verified when the chain tip changes
   - Rejected transactions must be properly tracked to prevent DoS attacks

2. **Performance**:
   - Transaction processing should be asynchronous
   - Memory usage should be bounded
   - Critical paths should be optimized for throughput

3. **Reliability**:
   - The mempool should recover from crashes and chain reorganizations
   - Background tasks should be resilient to temporary failures

## ZIP-401 Compliance

The mempool implements the requirements specified in [ZIP-401](https://zips.z.cash/zip-0401):

1. Implements `mempooltxcostlimit` configuration (default: 80,000,000)
2. Implements `mempoolevictionmemoryminutes` configuration (default: 60 minutes)
3. Uses random eviction when the mempool exceeds the cost limit
4. Caps eviction memory lists at 40,000 entries
5. Uses transaction IDs (txid) for version 5 transactions in eviction lists

## Error Handling

The mempool employs a comprehensive error handling strategy:

1. **Temporary Failures**: For network or resource issues, transactions remain in the queue for retry.

2. **Permanent Rejections**: For consensus or semantic failures, transactions are rejected with specific error reasons.

3. **Dependency Failures**: For missing inputs or dependencies, transactions may wait for dependencies to be resolved.

4. **Recovery**: On startup or after a crash, the mempool is rebuilt from scratch.

## Metrics and Diagnostics

The mempool provides metrics for monitoring:

1. **Transaction Count**: Number of transactions in the mempool
2. **Total Cost**: Total size of all mempool transactions
3. **Queued Count**: Number of transactions pending download or verification
4. **Rejected Count**: Number of rejected transactions in memory
5. **Background Task Status**: Health of crawler and queue checker tasks 
