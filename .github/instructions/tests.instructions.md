---
applyTo: "**/tests/**,**/tests.rs,**/prop.rs,**/vectors.rs,zebra-test/**"
---

# Safety review rules: tests

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Test both sides of every limit

Test size, count, and value limits exactly at the boundary and one past it, and assert the exact boundary condition. Cover both values of boolean protocol fields and the full range of input lengths the spec allows.

Look for: limit tests that only use values far from the limit; tests that check only the accepted or only the rejected side; boolean protocol fields tested with a single constant value; input lengths narrower than the spec allows

## Test consensus rules across heights, upgrades, and vectors

Test each consensus check on every transaction version, verification path (block and mempool), and NetworkUpgrade it applies to, on both sides of each activation height and at every affected height such as each halving. Use real protocol constants with spec or zcashd test vectors, and regenerate vectors only for a deliberate encoding change.

Look for: consensus checks tested on a single block; no blocks at activation height minus one and at activation height; property tests that generate one transaction version for all upgrades; checks tested on the block path only; no coinbase case, or no case with and without shielded data; versions not tested against their own sighash function; reward or subsidy changes without per-halving tests; NetworkUpgrade height changes without ordering tests; test data edited to make a failing test pass; placeholder constants in fixtures
