---
status: accepted
date: 2020-03-10
story: https://github.com/ZcashFoundation/zebra/issues/428
builds-on: RFC 0002 - Parallel Verification
---

# Three-Level Verification Pipeline

## Context and Problem Statement

Block and transaction verification involves many different checks. Some checks can run in parallel, while others require access to chain state. We need a verification architecture that maximizes parallelism while maintaining correctness.

## Priorities & Constraints

- Maximize parallel verification for throughput
- Separate concerns for maintainability
- Enable batch verification for cryptographic operations
- Fail fast on invalid data to avoid wasted work
- Clear mapping from verification stages to code locations

## Considered Options

- Option 1: Three-level verification (structural, semantic, contextual)
- Option 2: Two-level verification (syntax, semantics)
- Option 3: Single-pass verification

### Pros and Cons of the Options

#### Option 1: Three-Level Verification

- Good, because structural validity is enforced at parse time (zero runtime cost)
- Good, because semantic checks can run fully in parallel
- Good, because contextual checks are isolated to state service
- Good, because each level has clear ownership (chain/consensus/state crates)
- Bad, because the model requires understanding across multiple crates

#### Option 2: Two-Level Verification

- Good, because it's simpler to understand
- Bad, because it conflates parsing validity with cryptographic validity
- Bad, because less opportunity for parallelism

#### Option 3: Single-Pass Verification

- Good, because it's the simplest model
- Bad, because no parallelism opportunity
- Bad, because state access blocks cryptographic verification

## Decision Outcome

Chosen option: **Option 1: Three-Level Verification**

The three levels are:

1. **Structural Validity** (`zebra-chain`)
   - Enforced by Rust's type system at parse time
   - Invalid states are unrepresentable (e.g., can't construct a V2 tx with Sapling proofs)
   - Zero runtime cost after parsing

2. **Semantic Validity** (`zebra-consensus`)
   - Cryptographic verification: signatures, proofs, scripts
   - Can run fully in parallel across transactions
   - Uses batch verification for efficiency

3. **Contextual Validity** (`zebra-state`)
   - Chain state checks: UTXO availability, nullifier uniqueness
   - Must be checked sequentially per block
   - Requires database access

### Expected Consequences

- `zebra-chain` types guarantee structural validity by construction
- `zebra-consensus` can batch-verify all transactions in a block in parallel
- `zebra-state` performs final contextual checks before committing
- Clear error messages indicate which verification level failed
- New consensus rules map clearly to one of the three levels

## More Information

- [RFC 0002: Parallel Verification](https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html)
- [Design Overview: Verification Stages](https://zebra.zfnd.org/dev/overview.html)
