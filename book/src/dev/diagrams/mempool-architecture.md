# Mempool Architecture Diagram

This diagram illustrates the architecture of the Zebra mempool, showing its main components and the flow of transactions through the system.

```mermaid
graph TD
    %% External Components
    Net[Network Service]
    State[State Service]
    TxVerifier[Transaction Verifier]
    RPC[RPC Service]

    %% Mempool Main Components
    Mempool{{Mempool Service}}
    Storage{{Storage}}
    Downloads{{Transaction Downloads}}
    Crawler{{Crawler}}
    QueueChecker{{Queue Checker}}

    %% Transaction Flow
    Net -->|1- Poll peers| Mempool
    RPC -->|1- Direct submit| Mempool
    Crawler -->|1- Poll peers| Net
    Crawler -->|2- Queue transactions| Mempool

    Mempool -->|3- Queue for download| Downloads
    Downloads -->|4a- Download request| Net
    Net -->|4b- Transaction data| Downloads

    Downloads -->|5a- Verify request| TxVerifier
    TxVerifier -->|5b- Verification result| Downloads

    Downloads -->|6a- Check UTXO| State
    State -->|6b- UTXO data| Downloads

    Downloads -->|7- Store verified tx| Storage

    QueueChecker -->|8a- Check for verified| Mempool
    Mempool -->|8b- Process verified| QueueChecker

    Storage -->|9- Query responses| Mempool
    Mempool -->|10- Gossip new tx| Net

    %% State Management
    State -->|Chain tip changes| Mempool
    Mempool -->|Updates verification context| Downloads

    %% Mempool responds to service requests
    RPC -->|Query mempool| Mempool
    Mempool -->|Mempool data| RPC

    %% Styling
    classDef external fill:#444,stroke:#888,stroke-width:1px,color:white;
    classDef component fill:#333,stroke:#888,stroke-width:1px,color:white;

    class Net,State,TxVerifier,RPC external;
    class Mempool,Storage,Downloads,Crawler,QueueChecker component;
```

## Component Descriptions

1. **Mempool Service**: The central coordinator that handles requests and manages the mempool state. Located at `zebrad/src/components/mempool.rs`.

2. **Storage**: In-memory storage for verified transactions and rejection lists. Located at `zebrad/src/components/mempool/storage.rs`.

3. **Transaction Downloads**: Handles downloading and verifying transactions from peers. Located at `zebrad/src/components/mempool/downloads.rs`.

4. **Crawler**: Periodically polls 3 peers (FANOUT=3) every 73 seconds for new transaction IDs. Located at `zebrad/src/components/mempool/crawler.rs`.

5. **Queue Checker**: Triggers the mempool to process verified transactions every 5 seconds. It sends `CheckForVerifiedTransactions` requests but doesn't process responses directly - the Mempool's `poll_ready()` handles collecting verified transactions from the Downloads stream. Located at `zebrad/src/components/mempool/queue_checker.rs`.

6. **Gossip Task** (not shown): A separate async task that broadcasts new transaction IDs to peers via `MempoolChange` events. Located at `zebrad/src/components/mempool/gossip.rs`.

## Transaction Flow

1. Transactions arrive via network gossiping, direct RPC submission, or crawler polling. Both RPC and Crawler use the same `Request::Queue` mechanism to submit transaction IDs to the mempool.

2. The mempool checks if transactions are already known or rejected. If not, it queues them for download.

3. The download service retrieves transaction data from peers.

4. Transactions are verified against consensus rules using the transaction verifier.

5. Verified transactions are stored in memory and gossiped to peers via the Gossip task.

6. The queue checker triggers the mempool to process newly verified transactions from the Downloads stream.

7. Transactions remain in the mempool until they are mined or evicted due to size limits.

8. When the chain tip changes, the mempool updates its verification context and potentially evicts invalid transactions.
