# Block Lifecycle

A block enters Zebra and either commits to the best chain or is
rejected. The path is identical whether the block came from sync,
gossip, or an RPC `submitblock`:

```text
peer/rpc ─▶ inbound/syncer ─▶ block verifier router ─┐
                                                     ├─▶ state ─▶ best chain
                                                     │        └─▶ non-finalized side chain
                           (checkpoint or semantic) ─┘
```

1. **Receipt.** A block arrives either proactively (the syncer
   requested it based on a hash from a peer's `ObtainTips` response)
   or reactively (a peer gossiped an `inv`, the inbound service
   requested the block).
2. **Routing.** The `BlockVerifierRouter` inspects the block's height.
   Below the highest mandatory checkpoint height, it routes to the
   `CheckpointVerifier`; above it, to the `SemanticBlockVerifier`.
3. **Semantic verification.**
   - The checkpoint path performs a small, fixed set of checks
     (header linkage, target difficulty, transaction Merkle root) and
     relies on the hardcoded checkpoint hash to reject tampered data.
   - The semantic path performs the full set of context-free consensus
     rules: script execution, signature verification, zk-SNARK proof
     checks, value balance, note commitment updates, and so on. Heavy
     cryptographic work (Sapling/Orchard proofs, Groth16) is dispatched
     through `tower-batch-control`, which groups contemporaneous
     requests into a single batch for amortized verification, and runs
     inside `spawn_blocking` so it does not monopolize the async
     runtime.
4. **Contextual verification.** The state service receives either
   `CommitCheckpointVerifiedBlock` or `CommitSemanticallyVerifiedBlock`
   and performs the consensus rules that require chain history — UTXO
   spending, nullifier uniqueness, note commitment tree roots. Only
   then is the block added to a chain.
5. **Placement.** Every accepted block enters the in-memory
   non-finalized state, which tracks multiple possible chains (forks)
   up to the finality depth. Once a block is buried deep enough
   (~100 blocks), it is moved to RocksDB and becomes part of the
   finalized state. See [State](state.md) for the split in detail.
6. **Gossip.** If the block extended the best chain, it is relayed to
   peers via the gossip task.

## Why the Verifier/State Split

**Context-free rules** live in `zebra-consensus` and can run
concurrently across many blocks (and in parallel batches across many
proofs). **Contextual rules** live in `zebra-state` because only the
state knows the UTXO set, nullifier set, and note commitment trees.
Keeping these responsibilities separate means the verifier is mostly
pure and the state is mostly a database with rules.

## Checkpoint vs Semantic Verification

Zebra ships with a long list of **mandatory checkpoint hashes** for
blocks that have long-since finalized on mainnet. Up to the highest
checkpoint, new nodes verify blocks by hash linkage and basic header
sanity, trusting the checkpoint list for the rest.

The motivation is pragmatic:

- Re-verifying every historical zk-SNARK proof from genesis is
  expensive and offers no meaningful security improvement over a hash
  check against a publicly audited checkpoint list shipped with the
  source.
- It lets the initial sync hit high throughput without starving the
  network stack or blocking contextual rules on crypto batch filling.

The cost is a stronger release-engineering obligation: the checkpoint
list must be regenerated for each release (see
[Generating Zebra Checkpoints](../zebra-checkpoints.md)). A bad
checkpoint is a consensus bug.

Above the last checkpoint, full semantic verification is always on.
There is no "trust mode" — post-checkpoint blocks pay the full crypto
cost.
