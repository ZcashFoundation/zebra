---
applyTo: "zebra-network/**,zebra-node-services/**,zebrad/src/components/sync/**,zebrad/src/components/mempool/**,zebrad/src/components/inbound/**,zebrad/src/components/sync.rs,zebrad/src/components/mempool.rs,zebrad/src/components/inbound.rs"
---

# Safety review rules: network, P2P, mempool, and sync

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Bound every network wait with a named timeout

Put a timeout, defined as a named constant, on every wait that depends on a peer or the network, and route timeout handling through one shared helper. Classify every error type a timeout can produce, and count background connection attempts against the connection limit.

Look for: awaits on peer responses with no timeout; inline `Duration::from_secs(..)` literals; duplicated timeout handling across code paths; error matching that handles only one timer's error type; background connection attempts outside the connection limit

## Penalize the peer that supplied invalid data

Attribute misbehavior to the specific peer that sent invalid data once the outcome is known, and keep misbehavior scores intact through error wrapping. Count invalid responses in the same accounting as stalled peers, and credit peers only for validated responses.

Look for: misbehavior scores lost when errors are wrapped or converted; invalid responses handled differently from stalls; peers credited before validation completes; deterministic rejection errors not tracked as deterministic

## Match wire formats to the spec and zcashd

Implement wire formats, encodings, network magic, enum values, limits, and replies exactly as the protocol spec, ZIPs, and zcashd define them. Reject reserved values where the spec requires it, and keep ignoring unknown service bits as the version decoder does. Stay compatible with older peers, and advertise a protocol version only after its NetworkUpgrade activation height is scheduled.

Look for: a new protocol version constant advertised before activation is set; parsing that accepts values the spec says to reject; message fields that differ from zcashd behavior; changes that stop handling messages older peers still send
