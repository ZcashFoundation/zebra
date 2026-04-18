# Startup and Orchestration

`zebrad` itself contains very little business logic. Its main job is to
construct the services in the correct order, wire them together, and
keep them alive. The `start` command (see
`zebrad/src/commands/start.rs`) initializes components roughly in this
order:

1. **State.** Open RocksDB, construct the write-side `StateService` and
   the read-side `ReadStateService`, expose a `LatestChainTip` watch
   channel and a `ChainTipChange` broadcast.
2. **Network.** Build the peer set, address book, and the inbound
   service (initially a placeholder that will be swapped in once its
   downstreams exist).
3. **Verifiers.** Construct the `BlockVerifierRouter` (which routes
   each block to either the checkpoint or semantic verifier) and the
   transaction verifier. Spawn a background task that checks
   consistency between the hard-coded checkpoint list and the finalized
   state.
4. **Syncer.** Wire the chain syncer to the peer set, verifier, and
   state.
5. **Mempool.** Wire the mempool to state, tx verifier, peer set, and
   the sync-status watch.
6. **Inbound handoff.** Hand the now-complete set of downstream
   services to the inbound service via a oneshot channel.
7. **Long-running tasks.** Spawn JSON-RPC, indexer gRPC, block and
   transaction gossip, the mempool crawler, progress logging, health
   endpoints, and optionally the internal miner.

## Subtleties Worth Internalizing

- **The inbound service is created early but wired late.** Peers
  can connect and issue requests before the verifier and mempool
  exist. The inbound service buffers those requests until the oneshot
  handoff completes. This preserves the natural construction order
  (state → network → verifier → sync → mempool → inbound) without
  requiring callers to know about initialization phases.

- **Cross-component communication is mostly channels.** The state
  service exposes `watch::Receiver<Option<ChainTipBlock>>` for "what's
  the tip right now?" and a broadcast channel for "tip changed". This
  avoids Mutex sharing and lets any number of components subscribe to
  freshness signals.

- **Regtest is a small special case.** On regtest, the mempool is
  enabled from height 0 and the genesis block is committed directly,
  so local development does not require peer sync.
