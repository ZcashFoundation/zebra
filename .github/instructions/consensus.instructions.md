---
applyTo: "zebra-consensus/**,zebra-script/**,zebra-chain/src/parameters/**,zebra-chain/src/orchard/**,zebra-chain/src/sapling/**,zebra-chain/src/sprout/**,zebra-chain/src/primitives/**,zebra-chain/src/work/**"
---

# Safety review rules: consensus, script, and cryptography

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Implement consensus rules exactly as specified

Implement consensus checks, cryptography, and constants exactly as the protocol spec defines them, including strict versus inclusive bounds, with no added or loosened conditions. Where the spec's indexing differs from zcashd's consensus-critical behavior, match zcashd.

Look for: `<` versus `<=` in bound checks; range checks on partial sums instead of the final sum; hard-coded placeholder values in place of computed consensus values; validation stricter than the spec, such as rejecting valid addresses; embedded parameter data with no check against a trusted reference

## Use the configured network for network-specific values

Pass the network explicitly and read activation heights, branch IDs, address encodings, and limits from the configured network's parameters. Values a ZIP or the protocol spec fixes for every network stay constants, and only values that differ per network belong in network parameters.

Look for: `Network::Mainnet` or `Default::default()` in code that serves any network; consensus defaults derived from network names or checkpoint settings; a ZIP-fixed value made configurable per network; a Testnet or regtest value hardcoded as a Mainnet constant

## Match activation heights and constants to zcashd

Set network upgrade activation heights, protocol versions, consensus branch IDs, and consensus constants to the ZIP and zcashd values. Assign them only once a deployment ZIP establishes them, and replace provisional values as soon as it does.

Look for: new activation heights for unscheduled upgrades; branch IDs not yet set by a deployment ZIP; provisional constants the ZIP has since fixed; unreleased values not behind a `cfg` flag

## Gate upgrade rules on their own activation height

Gate each consensus rule and each piece of upgrade-specific data on the activation height of the NetworkUpgrade that introduces it. Keep existing activation timing unchanged.

Look for: checks gated on whichever upgrade comes next rather than the one that introduced the rule; upgrade-specific fields populated before activation; `NetworkUpgrade` comparisons that rely on enum declaration order; callers each checking activation instead of the consensus check doing it

## Validate fully before committing or mutating state

Run every fallible check, including checks on untrusted peer data, before committing, mutating, or acting on state, so a rejected operation leaves no partial update. Defer checks that need unavailable data, but run them before the block is committed to zebra-state.

Look for: inserts or removals before the last `?`; work published or precomputed before contextual checks pass; related indexes updated separately instead of through one type; deferred constraints never checked before commit

## Respect consensus, chain, and state layering

Keep data structures in zebra-chain, chain-state lookups in zebra-state, and context-free checks in the `check` modules of `zebra-consensus::block` and `zebra-consensus::transaction`. Run verification in the verifier services, such as `SemanticBlockVerifier` and `transaction::Verifier`, so every validation path applies the same checks.

Look for: height-independent or state-independent checks added to contextual validation such as `zebra-state::service::check`; verification logic on `Request` or `Response` types instead of a verifier service; stateless helpers outside a `check` module; a new contextual path that skips checks the existing path performs

## Run cheap checks first

Order checks from cheapest to most expensive, and run slow independent checks concurrently in the async check batch.

Look for: proof or signature verification before cheap synchronous checks; key derivation before precondition checks; slow independent checks outside the async check batch
