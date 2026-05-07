# RPC

Zebra exposes two RPC surfaces:

- **JSON-RPC 2.0 over HTTP**, modeled on `zcashd` for wallet and tool
  compatibility. Read methods (`getblock`, `getrawtransaction`,
  `getblockcount`, etc.) hit the read-side state service. Write-ish
  methods (`sendrawtransaction`) go through the mempool. Mining
  methods (`getblocktemplate`, `submitblock`) cross-cut state,
  mempool, and verifier.
- **Indexer gRPC**, a separate server for block/transaction indexing
  clients that need richer, streaming access.

## `zcashd` Compatibility Is a Constraint

`zcashd` compatibility is a non-negotiable constraint on the JSON-RPC
surface. Error codes, field names, and edge-case behavior must match
the reference implementation closely enough that existing wallets work
without modification. This occasionally forces ugly compromises — the
compatibility bar outweighs ergonomic improvements.

New RPC methods or deviations from `zcashd` require strong
justification and are typically opt-in. "We could do this more
cleanly" is not by itself a sufficient reason to diverge.

## The `getblocktemplate` Flow

The `getblocktemplate` flow is the most entangled in the RPC layer:

1. The RPC handler snapshots the chain tip.
2. It assembles a candidate template from the mempool using ZIP-317
   fee rules.
3. It hands the result to the caller.
4. A subsequent `submitblock` call routes through the block verifier
   (in a special "proposal" mode that skips proof-of-work verification
   if the caller requested it).
5. If valid, the block is gossiped through the normal inbound path.

Because mining calls hit state, mempool, and verifier simultaneously,
changes to any of those components need to be evaluated against the
`getblocktemplate` path specifically — not just the sync path.
