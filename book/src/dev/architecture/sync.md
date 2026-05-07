# Sync: Pipelined Block Lookup

Chain synchronization is built on the pattern described in
[RFC 0001 — Pipelinable Block Lookup](../rfcs/0001-pipelinable-block-lookup.md).
At a high level:

1. Ask a small fanout of peers for tips via a **block locator**
   (a sparse hash ladder back through the known chain). Peers respond
   with a sequence of hashes extending their best chain.
2. Dispatch the returned hashes as download requests, spread across
   peers, with **hedging** (issue a second request after a short
   delay if the first is slow) and **retries**.
3. Feed downloaded blocks into the verifier/state pipeline with
   concurrency limits: a larger limit for the checkpoint path (where
   blocks are cheap to verify) and a smaller one for the semantic
   path (where they are expensive).
4. Once the chain tip is caught up, step back and let the inbound
   path (peer gossip) drive further updates.

## Policy as Tower Layers

Hedging and retries are applied as Tower layers around the network
service, not inside it. This is a recurring pattern in Zebra: the
network crate provides a minimal "send this request to a peer"
primitive, and higher-level policy is composed around it.

The syncer's stack looks approximately like:

```text
Hedge<ConcurrencyLimit<Retry<Timeout<Network>>>>
```

Each layer is independently configurable and independently testable.
Policy changes (retry count, hedge delay, concurrency window) become
tuning knobs rather than code restructuring.

## Failure Handling

A single slow or malicious peer cannot stall sync: the hedge copy
races against the original request, and a retried request can be
routed to a different peer. Transient errors are handled without user
intervention. On unrecoverable errors, the syncer cancels all pending
downloads and verifications and restarts from `ObtainTips`.
