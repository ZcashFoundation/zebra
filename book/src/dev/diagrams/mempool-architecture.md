# Mempool Architecture Diagram

This diagram illustrates the architecture of the Zebra mempool, showing its main components and the flow of transactions through the system.

```mermaid
graph TD
    %% External Components
    Net[("Network Service")]
    State[("State Service")]
    TxVerifier[("Transaction Verifier")]
    RPC[("RPC Service")]
    
    %% Mempool Main Components
    Mempool{{"Mempool Service"}}
    Storage{{"Storage"}}
    Downloads{{"Transaction Downloads"}}
    Crawler{{"Crawler"}}
    QueueChecker{{"Queue Checker"}}
    
    %% Transaction Flow
    Net -->|1. Inv messages| Mempool
    RPC -->|1. Direct submit| Mempool
    Crawler -->|1. Poll peers| Net
    Crawler -->|2. Queue transactions| Mempool
    
    Mempool -->|3. Queue for download| Downloads
    Downloads -->|4a. Download request| Net
    Net -->|4b. Transaction data| Downloads
    
    Downloads -->|5a. Verify request| TxVerifier
    TxVerifier -->|5b. Verification result| Downloads
    
    Downloads -->|6a. Check UTXO| State
    State -->|6b. UTXO data| Downloads
    
    Downloads -->|7. Store verified tx| Storage
    
    QueueChecker -->|8a. Check for verified| Mempool
    Mempool -->|8b. Process verified| QueueChecker
    
    Storage -->|9. Query responses| Mempool
    Mempool -->|10. Gossip new tx| Net
    
    %% State Management
    State -->|Chain tip changes| Mempool
    Mempool -->|Updates verification context| Downloads
    
    %% Mempool responds to service requests
    RPC -->|Query mempool| Mempool
    Mempool -->|Mempool data| RPC
```

## Component Descriptions

1. **Mempool Service**: Central coordinator handling requests and managing mempool state.

2. **Storage**: In-memory storage for verified and rejected transactions.

3. **Transaction Downloads**: Handles downloading and verifying transactions.

4. **Crawler**: Polls peers for new transactions.

5. **Queue Checker**: Polls for newly verified transactions.

## Transaction Flow

1. Transactions arrive via network gossiping, direct RPC submission, or crawler polling.

2. Mempool checks if transactions are known or rejected, queues new ones for download.

3. Download service retrieves transaction data from peers.

4. Transactions are verified against consensus rules.

5. Verified transactions are stored and gossiped to peers.

6. Queue checker regularly checks for newly verified transactions.

7. Transactions remain in mempool until mined or evicted.

8. Chain tip changes trigger verification context updates. 
