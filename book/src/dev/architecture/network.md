# Network: Per-Peer State Machines

`zebra-network` has two external faces:

- **The managed peer set** — an auto-growing, auto-healing pool that
  load-balances outbound requests and routes inbound requests to a
  local service.
- **`connect_isolated`** — a single connection with no dependence on
  other node state, useful for anonymity-preserving relay (e.g. over
  Tor) or for building crawlers that must not leak node fingerprints.

## Internal Protocol Translation

Inside, each connected peer runs its own state machine. The machine
translates between the external Zcash wire protocol (inherited from
Bitcoin: `inv`, `getdata`, `addr`, `ping`, block and transaction
messages) and an internal stateless request/response protocol.

The translation layer matters because it means the rest of the node
never sees raw wire messages. A block-download request from the
syncer is a single internal `BlocksByHash` request; it may be satisfied
by whichever peer(s) have the block, with the network crate handling
timeouts, fan-out, and peer selection.

Because the wire and internal protocols are decoupled here, changes to
the Zcash wire protocol are local to `zebra-network`. The rest of the
node never has to learn that the wire format descends from Bitcoin.

## Peer Misbehavior

Peer misbehavior (malformed messages, invalid blocks, protocol
violations) feeds into a scoring system that can disconnect and
blacklist offenders. This is a defense-in-depth measure: the verifier
already rejects bad blocks, but silently ignoring misbehaving peers
would let a single bad actor waste sync bandwidth indefinitely.
