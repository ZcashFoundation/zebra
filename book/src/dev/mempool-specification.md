# Mempool specification

The [`Mempool` service][mempool] stores verified, unmined transactions in memory
for relay, RPC queries, and block templates. Admission does not confirm a
transaction, reserve a place in a block, or persist it across restarts. Shared
request and response types live in [`zebra-node-services`][api].

The [architecture diagram](diagrams/mempool-architecture.md) shows component
boundaries; [verification stages](overview.md#verification-stages) distinguish
consensus checks from relay policy.

## Activation

The service starts disabled. Activation requires a chain-tip update and either:

- a `SyncStatus` indicating that Zebra is close to the network tip; or
- a best-chain height at or above `[mempool] debug_enable_at_height`.

After activation, a later sync-status change alone does not disable the service.
A reset recreates an already-active mempool at the new tip regardless of sync
status.

The crawler, queue checker, and gossip task are spawned by
[node startup][start-tasks], not by the activation transition. The
[crawler][crawler] waits for close-to-tip status or the debug-height condition
before each crawl. This is separate from the service's one-time activation gate.

While disabled, the service returns empty ID, transaction-list, rejection-list,
and statistics responses. It rejects full-transaction, dependency, and output
queries. A `Queue` response contains a `Disabled` error per candidate.
The `QueueFromPeer` request silently drops candidates, avoiding disconnections.

## Configuration

For limits, defaults, and the Regtest activation override, see
[mempool configuration](../user/config.md#mempool).

## Candidate admission and verification

The service accepts candidates through three paths:

- The [inbound handler][inbound] uses `QueueFromPeer` for both advertised IDs and
  complete transactions received from a known peer.
- The [crawler][crawler] requests `MempoolTransactionIds` from the network service
  and submits the returned IDs with `Queue`.
- RPC submits complete transactions through `Queue`. The RPC layer also has a
  separate, bounded [retry queue][rpc-queue]; it is not the mempool download queue.

The [download pipeline][downloads] performs these steps:

1. Check storage for an existing transaction or cached rejection. Deduplicate
   outstanding requests by exact unmined ID. Variants with the same mined ID
   share a pending slot only when supplied by the same advertising IP address;
   one host's authorization variant cannot reserve other hosts' or local variants.
   Release reservations when tasks complete, fail, time out, or finish cancellation.
2. Enforce a global limit of **500** concurrent download/verification tasks and
   a limit of **5 per advertising IP address**. Multiple connections from the
   same IP share that allowance. Crawler and local submissions have no source IP
   and are subject only to the global limit.
3. Query chain state for an already-mined transaction and the current tip height.
   If the candidate contains only an ID, request its bytes with
   `TransactionsById`. A complete transaction skips that network download.
4. Call `MempoolTxVerifier` with the candidate and the next block height. Bound
   the operation with download, verification, and whole-task timeouts.
5. Return a `VerifiedUnminedTx`, its mempool-output dependencies, and the tip
   height used for verification to the mempool service.

The [transaction verifier][verifier] rejects coinbase transactions and applies
height-dependent [consensus checks][protocol-tx]. It obtains unspent transparent
outputs from the best chain or waits for a mempool parent through `AwaitOutput`.
It checks maturity and lock time, applies input standardness and fee policy
before expensive cryptography, verifies scripts, signatures, and proofs, and
queries state for valid anchors and unrevealed nullifiers.

The verifier waits up to **60 seconds** for a missing mempool output.
The [`PendingOutputs` tracker][pending] wakes waiters when storage inserts the
parent's output. An `AwaitOutput` response is informational: it does not guarantee
that the output remains unspent. Storage checks conflicts and dependency
availability again at insertion.

The [mempool's `poll_ready` method][mempool] collects completed download tasks. If
the chain grew during verification, it requeues the returned candidate for the
new context rather than inserting the stale result. A matching-context result
passes through `Storage::insert`; only successful storage admission completes a
queued caller's result with success. A successful initial `Queue` response alone
means the task was admitted, not that the transaction entered the mempool.

## Storage and standardness

The [`Storage` type][storage] owns the mined-ID-indexed [`VerifiedSet`][verified],
created and spent transparent outputs, nullifiers, transparent dependencies,
pending-output requests, and tip- or chain-scoped rejection caches.

Storage contains at most one transaction for each mined ID. Before insertion,
it rejects cached failures, duplicates, non-standard transactions, conflicts
with existing mempool spends or nullifiers, and transactions whose unconfirmed
parent outputs disappeared during verification. Removing a transaction also
removes transactions that depend on its outputs, directly or indirectly.

The [standardness checks][storage] require standard transparent scripts,
push-only input scripts within the size limit, standard input spending patterns,
and no dust outputs. They reject bare multisig outputs, more than one
`OP_RETURN` output, an `OP_RETURN` script above the configured data-carrier limit,
and more than **4,000** transparent signature operations per transaction.
These are mempool policy, not extra block-consensus rules.

### Fee policy and ZIP 317

Zebra 7.0.0 uses [ZIP 317][zip317]'s logical-action accounting with the
[proposed fee reduction][fee-proposal] to a
[`MARGINAL_FEE` of 1,000 zatoshis][fees]. Published ZIP 317 still specifies 5,000.
The mempool already accepts the reduced fee; the
[`getstandardfee` recommendation](../user/rpc.md) follows its deployment schedule.

The conventional fee is 1,000 times the greater of two and the transaction's
logical-action count. The zero `BLOCK_UNPAID_ACTION_LIMIT` rejects unpaid
actions, requiring at least this fee.

The [fee checker][fees] also retains a serialized-size fee floor of
`clamp(100 * size / 1000, 100, 1000)` zatoshis, using integer division and size in
bytes. With the unpaid-action limit set to zero, this floor cannot reject a
transaction that passed the conventional-fee check.

## Cost limit and eviction

The [ZIP 401][zip401] weighted eviction policy uses serialized transaction size
as its memory-size approximation. Zebra [calculates][unmined]:

```text
cost = max(serialized_size_in_bytes, 10000)
low_fee_penalty = 40000 if fee < implementation_conventional_fee, otherwise 0
eviction_weight = cost + low_fee_penalty
```

The penalty affects selection probability, not total cost. Admitted transactions
already pay the conventional fee, so their penalty is zero.

After inserting a candidate, [storage][storage] repeatedly selects a transaction
with probability proportional to its eviction weight until total cost is at or
below `[mempool] tx_cost_limit`. The new candidate participates in that selection
and can be evicted immediately. The [verified set][verified] removes the selected
transaction and its descendants. Storage records the selected transaction's
mined ID in the random-eviction rejection list; dependent removals do not each
receive their own random-eviction entry.

The [default cost limit](../user/config.md#mempool) permits at most
**8,000 transactions**, fewer when costs exceed 10,000. This is not an
80-megabyte process-memory limit: deserialized objects, indexes, rejection
caches, and in-flight candidates also consume memory.

## Rejection caches

The [storage error types][storage] distinguish authorizing data from transaction
effects. For witnessed transactions, an unmined ID includes the authorization
digest as well as the mined ID; the mined ID identifies the effects. See
[transaction identifiers][protocol-ids] and Zebra's [identifier types][unmined].

| Cache | Key | Examples | Lifetime |
| --- | --- | --- | --- |
| `tip_rejected_exact` | `UnminedTxId` | Failed consensus verification or standardness. | Cleared on best-tip growth or reset; cleared if it exceeds 40,000 entries. |
| `tip_rejected_same_effects` | Mined transaction ID | A mempool spend conflict or a missing unconfirmed parent output. | Cleared on best-tip growth or reset; cleared if it exceeds 40,000 entries. |
| `chain_rejected_same_effects` | Mined transaction ID, grouped by reason | Mined, expired, duplicate spend in the chain, or randomly evicted. | Each reason has a timed `EvictionList`, bounded to 40,000 entries; all are discarded on reset. |

The [`EvictionList`][eviction] drops its oldest entry at capacity, treats entries
older than `[mempool] eviction_memory_time` as absent, and prunes them on
insertion and mutable length queries.

Network download failures, state-query errors, cancellations, and whole-task
timeouts are not cached. Failed tasks leave the queue without automatic retry;
a later crawl or advertisement can resubmit them. Errors returned by the
verifier, including its timeout, are cached for the exact tip.

## Chain-tip changes

The [mempool service][mempool] processes tip changes before accepting another
request:

- **Growth by one block:** cancel pending work for mined transactions, remove
  mined transactions and conflicts with the block's spends or nullifiers, update
  dependency tracking, and clear tip-scoped rejection caches. Transactions whose
  parents were mined can remain because those outputs now belong to chain state.
- **Reset:** a changed best chain, skipped tip updates, or an activated network
  upgrade clears storage and rejection caches and cancels old downloads. The
  service then attempts to requeue its previously stored and pending candidates
  against the new tip. Requeueing remains subject to download-queue capacity.
- **Expiry:** remove transactions whose expiry height is at or below the current
  tip, together with their dependent transactions. They cannot be included in the
  next block. See [ZIP 203][zip203] and the
  [expiry-removal implementation][storage].

Ordinary growth does not reverify every stored transaction; it performs the
targeted removals above.

## Crawling, queue checks, and gossip

The [crawler][crawler] issues **three** `MempoolTransactionIds` requests per crawl,
not necessarily to distinct peers. Each response has a **6-second** timeout,
the crawl has a **73-second** timeout, and the crawler sleeps **73 seconds after
completion** before its next enabled crawl.

The [queue checker][checker] sleeps **5 seconds** between
`CheckForVerifiedTransactions` calls. The request causes the buffered service to
run `poll_ready`, where completion processing occurs. The transaction verifier
also schedules a best-effort check after a **50-millisecond** delay to reduce
latency for dependencies and RPC submission. Neither interval is a guaranteed
transaction-admission deadline.

The [gossip task][gossip] consumes `MempoolChange` events and advertises only
added transactions with `AdvertiseTransactionIds`. It combines available events
before sending and rate-limits successive broadcasts. Mined and invalidated
events are available to subscribers but are not transaction advertisements.
If a subscriber falls behind the bounded broadcast channel, it can miss events;
the event stream is not a durable transaction log.

## Service interface

The [`Request` and `Response` enums][api] define the internal Tower API.

| Request | Result or action |
| --- | --- |
| `TransactionIds` | Return the stored unmined IDs. |
| `TransactionsById` | Return transactions whose full unmined IDs match, including the authorization digest. |
| `TransactionsByMinedId` | Look up transactions using mined IDs. |
| `TransactionWithDepsByMinedId` | Return a verified transaction and its recorded dependency IDs. |
| `FullTransactions` | Return verified transactions, dependency information, and the last processed tip hash. |
| `RejectedTransactionIds` | Return supplied IDs matching a cached rejection. |
| `AwaitOutput` | Wait for a transparent output created by a mempool transaction; callers must impose a timeout. |
| `UnspentOutput` | Report whether an output is created or spent in the mempool, or unknown there. |
| `Queue` | Admit candidates and return per-candidate initial errors or one-shot receivers for the eventual result. |
| `QueueFromPeer` | Admit peer-tagged candidates under the per-IP cap; return an empty `Queued` vector rather than per-candidate receivers. |
| `CheckForVerifiedTransactions` | Trigger readiness processing and acknowledge the check. |
| `QueueStats` | Return transaction count, serialized bytes, the current usage estimate, and notification status. |

The [storage lookup][storage] resolves `TransactionsById` through the mined-ID
index, then checks the full unmined ID. A different authorization digest does
not match. The [statistics handler][mempool] sets `usage` equal to serialized
bytes and leaves `fully_notified` unset. Neither field is a measurement of all
mempool allocations or proof that every notification was delivered.

## Metrics

The [metrics endpoint](../user/metrics.md) converts these recorder names to
Prometheus form:

| Metric | Meaning | Source |
| --- | --- | --- |
| `zcash.mempool.size.transactions` | Stored transaction count. | [Verified set][verified] |
| `zcash.mempool.size.bytes` | Sum of serialized sizes. | [Verified set][verified] |
| `zcash.mempool.cost.bytes` | Sum of ZIP 401 transaction costs. | [Verified set][verified] |
| `mempool.currently.queued.transactions` | Current download/verification queue length. | [Downloads][downloads] |
| `mempool.rejected.transaction.ids` | Entries in rejection caches; an ID in multiple lists is counted multiple times. | [Storage][storage] |
| `mempool.full_queue.per_peer.total` | Admission attempts rejected by the per-IP queue cap. | [Downloads][downloads] |
| `mempool.failed.verify.tasks.total` | Failed task count, labeled by reason. | [Mempool][mempool] |
| `mempool.gossiped.transactions.total` | Transaction IDs submitted to network broadcasts. | [Gossip][gossip] |

[mempool]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool.rs
[api]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-node-services/src/mempool.rs
[start-tasks]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/commands/start.rs#L625-L684
[inbound]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/inbound.rs#L595-L625
[crawler]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/crawler.rs
[downloads]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/downloads.rs
[verifier]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/transaction.rs
[pending]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/pending_outputs.rs
[storage]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/storage.rs
[verified]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/storage/verified_set.rs
[eviction]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/storage/eviction_list.rs
[fees]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/transaction/unmined/zip317.rs
[unmined]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/transaction/unmined.rs
[rpc-queue]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/src/queue.rs
[checker]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/queue_checker.rs
[gossip]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/mempool/gossip.rs
[zip203]: https://zips.z.cash/zip-0203
[zip317]: https://zips.z.cash/zip-0317
[fee-proposal]: https://github.com/zcash/zips/pull/1352
[zip401]: https://zips.z.cash/zip-0401
[protocol-tx]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
[protocol-ids]: https://zips.z.cash/protocol/protocol.pdf#txnidentifiers
