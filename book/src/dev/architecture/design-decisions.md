# Design Decisions

This page captures choices that are not obvious from the code, so that
future contributors know *why* before they propose changes. Each entry
follows the same shape: **Decision / Why / Implications**.

New decisions that affect the shape of the system overall belong here.
Decisions with deeper rationale, alternatives considered, or narrower
scope should be written up as an [RFC](../rfcs.md) and linked from
here.

## D1. Validator Scope Only

*Decision.* Zebra will not implement wallet, block explorer, or light
client functionality.

*Why.* Narrow scope keeps the trusted computing base small and lets
the team specialize the codebase for validation performance and
correctness. Applications belong in separate processes with their own
release cadence and attack surface.

*Implications.* Feature requests that widen scope are redirected to
Zaino, Zallet, or librustzcash. RPC additions that exist only to
serve wallet features are held to a high bar.

## D2. Library-First Crate Structure

*Decision.* Every major responsibility is a reusable crate with a
public Request/Response interface; `zebrad` only orchestrates.

*Why.* A monolithic node makes it hard to reuse the network or
verification stack outside the full node, and couples component
lifetimes to the node process. A library-first design makes
`zebra-network` usable from a crawler and `zebra-consensus` usable
from an offline validator, without forking the code.

*Implications.* Cross-crate APIs should evolve slowly and deliberately.
Breaking them is expensive.

## D3. Microservices in One Process

*Decision.* All stateful components expose `tower::Service`
interfaces, even though they run in the same process.

*Why.* Backpressure, timeouts, retries, hedging, load shedding, and
batch control become composable middleware. The same request/response
discipline that works at a network boundary works here, and the code
is ready to be split across processes later if we ever need to.

*Implications.* Keep Request/Response enums minimal and stable. Never
add a side channel (shared mutable state, global singletons) to
bypass the service boundary — it defeats the backpressure story.

## D4. `zebra-chain` Is Sync-Only

*Decision.* No async, no tokio, no Tower types in `zebra-chain`.

*Why.* The core types and serializers must be usable from tools and
downstream libraries that do not want to pull in an async runtime.
Keeping parsing logic synchronous also keeps it easy to reason about
for consensus correctness.

*Implications.* Any async coordination involving chain types happens
in the crates that depend on `zebra-chain`, not inside it.

## D5. Checkpoints Up to a Recent Height

*Decision.* Ship hardcoded block hashes and skip full verification
up to the highest mandatory checkpoint.

*Why.* Re-verifying historical zk-SNARKs from genesis is expensive
and does not materially increase security over checking against a
public, auditable list. The throughput win during initial sync is
large.

*Implications.* The checkpoint list is release-critical. It must be
regenerated, reviewed, and tested on every release; a wrong entry is
a consensus bug.

## D6. Two-Tier State

*Decision.* Non-finalized blocks live in an in-memory chain forest;
only finalized blocks go to RocksDB.

*Why.* Reorgs are cheap when forks are in-memory and share structure.
RocksDB is optimized for append-only immutable data, not for
rollback. The boundary between "can reorg" and "cannot reorg" matches
cleanly to "in memory" vs "on disk".

*Implications.* Queries that span both tiers must handle the boundary
explicitly. Finalized depth is a consensus-relevant constant.

## D7. Read/Write Service Split

*Decision.* `StateService` (write) and `ReadStateService` (read) are
separate services over the same underlying storage.

*Why.* Long-running RPC queries should not stall block commits, and
block commits should not be held up by opportunistic readers. RocksDB
snapshots make this cheap.

*Implications.* The write service is the only source of truth for
mutations; readers must tolerate seeing a slightly stale snapshot.

## D8. Per-Peer State Machines and an Internal Protocol

*Decision.* Each peer has its own state machine; the rest of the node
speaks an internal stateless request/response protocol.

*Why.* Decouples peer protocol complexity (versioning, handshakes,
flow control) from the rest of the node. The verifier and syncer do
not need to know that Zcash's wire protocol descends from Bitcoin's.

*Implications.* Protocol changes are local to `zebra-network`. The
internal protocol can evolve independently of the wire protocol, and
vice versa.

## D9. Pipelined Block Lookup with Hedging and Retries

*Decision.* Sync downloads are composed of hedged, retried,
concurrency-limited, timed requests against a fan-out of peers.

*Why.* A single slow or malicious peer cannot stall sync. Bandwidth
is spread across the peer set. Retries handle transient errors
without user intervention.

*Implications.* Network consumers should route through the peer set's
load-balancing layer, not grab a specific peer directly.

## D10. Mempool Activates Only Near Tip

*Decision.* The mempool does not accept transactions while the node
is catching up.

*Why.* A mempool built against an old state is worse than no mempool:
transactions would be verified against stale UTXO and nullifier sets,
and many would have to be re-verified anyway once sync completed.

*Implications.* Users running a fresh node will not see transaction
relay until sync is close to tip, which is expected behavior.

## D11. `zcashd` JSON-RPC Compatibility Is Load-Bearing

*Decision.* The JSON-RPC surface matches `zcashd` behavior in field
names, error codes, and edge cases, even when that behavior is
awkward.

*Why.* Zebra replaces `zcashd` in existing deployments. Wallets,
explorers, and tools must keep working unchanged.

*Implications.* Deviations from `zcashd` require strong justification
and are typically opt-in. "We could do this more cleanly" is not by
itself a sufficient reason to diverge.

## D12. Batch Verification of Cryptographic Primitives

*Decision.* Signature and proof verification goes through
`tower-batch-control`, which groups contemporaneous requests.

*Why.* Several of the cryptographic primitives used in Zcash have
batch verification algorithms that are materially faster per item
than single-item verification, particularly for zk-SNARK proofs.
Batching on the async boundary gets us those wins transparently.

*Implications.* New cryptographic verification paths should plug into
the batch control system rather than verifying items one at a time.

## D13. No Consensus-Relevant State Behind a `Mutex`

*Decision.* Shared async state uses `watch` and channel broadcasts
rather than `Mutex`-guarded singletons.

*Why.* Mutex-guarded state makes it too easy to read stale values
racily or to deadlock between tasks. Watch channels make "latest
value" an explicit, observable, subscribable signal.

*Implications.* When you are tempted to reach for `Arc<Mutex<T>>`
for cross-task communication, ask whether a channel or watch fits
instead.
