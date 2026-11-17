---
applyTo: "zebra-chain/**"
---

# Safety review rules: chain data and serialization

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Serialize consensus data in spec-defined layout

Serialize consensus structures exactly as the Zcash protocol spec and the governing ZIP define them, including field order, separately encoded arrays, and per-version field presence, rather than following the in-memory struct shape, a derived encoding, or what the test vectors happen to exercise.

Look for: `zcash_serialize` writing fields in struct declaration order rather than spec order; `#[derive(Serialize)]` or serde used for consensus bytes; arrays interleaved per element where the spec encodes them as separate arrays; transaction digest or sighash code with field order that differs from the ZIP
