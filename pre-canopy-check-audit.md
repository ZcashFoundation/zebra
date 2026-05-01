# Pre-Canopy Consensus Check Audit

**Issue**: [#10184](https://github.com/ZcashFoundation/zebra/issues/10184)
**Date**: 2026-03-30
**Scope**: Consensus checks in `zebra-consensus`, `zebra-state`, and `zebra-chain` that are NOT enforced for pre-Canopy blocks

## Background

Zebra uses a mandatory checkpoint at the block just before Canopy activation (`zebra-chain/src/parameters/network.rs:256-263`). All blocks at or below this height are verified by the `CheckpointVerifier`, which validates (`zebra-consensus/src/checkpoint.rs:579-633`):

- Block hash matches a hardcoded checkpoint (if a checkpoint exists at that height)
- Block height is sequential (coinbase height extraction)
- Proof of work: difficulty threshold and equihash solution (`checkpoint.rs:601-611`)
- Merkle root validity and duplicate transaction detection (`checkpoint.rs:626-630`)
- Subsidy-related deferred pool balance changes (`checkpoint.rs:614-621`)

The `CheckpointVerifier` **skips contextual and most semantic verification checks** (`zebra-consensus/src/checkpoint.rs:1-15`). This means checks in the `SemanticBlockVerifier` path — such as proof verification, script verification, and most upgrade-gated rules — are not enforced for pre-Canopy blocks.

Even with `checkpoint_sync = false`, the mandatory checkpoint height (just before Canopy) is always used as the minimum checkpoint height (`zebra-consensus/src/router.rs:401-406`).

## Findings

### 1. Coinbase Output Descriptions (Pre-Heartwood)

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:156-189` |
| **Rule** | Pre-Heartwood: coinbase transactions MUST NOT have any Output descriptions |
| **Gate** | Heartwood activation (explicitly documented as NOT VALIDATED) |
| **Risk** | **Documented gap** — Zebra explicitly notes at line 168-170: "Zebra does not validate this last rule explicitly because we checkpoint until Canopy activation." |

### 2. Shielded Coinbase Output Decryption

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:307-366` |
| **Rule** | Heartwood+: Sapling and Orchard coinbase outputs MUST decrypt with the all-zeros outgoing viewing key (ZIP-212) |
| **Gate** | Heartwood activation (line 348-354: returns `Ok(())` for pre-Heartwood) |
| **Risk** | Low — pre-Heartwood coinbase transactions should not have shielded outputs (see #1), so this check is moot for pre-Heartwood. For Heartwood-to-Canopy blocks, this is skipped by checkpoint. |

### 3. Sprout Pool Closure

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:215-246` |
| **Rule** | Canopy+: `vpub_old` MUST be zero (no new value entering Sprout pool) |
| **Gate** | Canopy activation (line 234) |
| **Risk** | None for pre-Canopy — the Sprout pool was open pre-Canopy by design. |

### 4. Subsidy Validation: Founders Reward vs Funding Streams

| | |
|---|---|
| **File** | `zebra-consensus/src/block/check.rs:156-303` |
| **Rule** | Pre-Canopy and pre-first-halving: validate Founders Reward outputs. Canopy+: validate Funding Streams (ZIP-1014). |
| **Gate** | Canopy activation (line 190) and first halving height (line 204) |
| **Risk** | Low — the Founders Reward check only applies for heights before the first halving (`height < net.height_for_first_halving()`). On Testnet, there is a pre-Canopy range after the first halving where neither the Founders Reward nor Funding Stream checks apply. This validation is skipped entirely for checkpoint-verified blocks. |

### 5. Coinbase Expiry Height (NU5)

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:368-407` |
| **Rule** | NU5+: coinbase `nExpiryHeight` MUST equal block height. Pre-NU5: only validates expiry_height <= 499999999. |
| **Gate** | NU5 activation (lines 380-387) |
| **Risk** | None for pre-Canopy — this is a post-NU5 rule. |

### 6. Consensus Branch ID for V5 Transactions

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:519-558` |
| **Rule** | NU5+: V5 transactions must have correct `nConsensusBranchId` |
| **Gate** | NU5 activation (line 545: returns `Ok(())` for pre-NU5) |
| **Risk** | None for pre-Canopy — V5 transactions don't exist pre-NU5. |

### 7. V4 Transaction Network Upgrade Support

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction.rs:920-961` |
| **Rule** | V4 transactions supported from Sapling through NU6.1 |
| **Gate** | Sapling activation |
| **Risk** | None for pre-Canopy — V4 transactions existing pre-Sapling would be caught by other validation, and are checkpointed. |

### 8. V5 Transaction Network Upgrade Support

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction.rs:1010-1048` |
| **Rule** | V5 transactions only supported from NU5 onward |
| **Gate** | NU5 activation (line 1028-1031) |
| **Risk** | None for pre-Canopy — V5 transactions don't exist pre-NU5. |

### 9. Miner Fee Exactness (NU6)

| | |
|---|---|
| **File** | `zebra-consensus/src/block/check.rs:305-361` |
| **Rule** | NU6+: coinbase total_output_value MUST equal total_input_value (exact). Pre-NU6: total_output_value must not exceed total_input_value. |
| **Gate** | NU6 activation (line 352) |
| **Risk** | None for pre-Canopy — this is a post-NU6 rule. |

### 10. NU6.1 Lockbox Disbursements

| | |
|---|---|
| **File** | `zebra-consensus/src/block/check.rs:261-282` |
| **Rule** | At NU6.1 activation height: validate one-time lockbox disbursement outputs (ZIP-271, ZIP-1016) |
| **Gate** | NU6.1 activation height (line 261) |
| **Risk** | None for pre-Canopy — this is a post-NU6.1 rule. |

### 11. Block Header Commitment Validation (Sapling Root, Chain History Root)

| | |
|---|---|
| **File** | `zebra-state/src/service/check.rs:137-222` and `zebra-chain/src/block/commitment.rs:108-151` |
| **Rule** | Sapling/Blossom: `hashLightClientRoot` MUST equal the Sapling note commitment tree root. Heartwood/Canopy: `hashLightClientRoot` MUST equal the chain history root (ZIP-221). NU5+: `hashBlockCommitments` MUST match (ZIP-244). |
| **Gate** | Upgrade-dependent interpretation of the block header commitment field |
| **Risk** | **Documented gap** — The contextual equality check in `service/check.rs:155` explicitly states: "We don't need to validate this rule since we checkpoint on Canopy." The pre-Sapling, Sapling/Blossom, and Heartwood activation reserved cases all return `Ok(())` without contextual validation. Note: structural parsing of the commitment bytes IS performed at the `zebra-chain` level (`block/commitment.rs:116-151`) — Sapling/Blossom bytes must parse as a valid Sapling root, and Heartwood activation bytes must be all-zeros. This structural validation happens during deserialization regardless of the verification path. The gap is specifically in the contextual check that the parsed value matches the expected tree root. |

### 12. Non-Coinbase Expiry Height Validation

| | |
|---|---|
| **File** | `zebra-consensus/src/transaction/check.rs:414-439` |
| **Rule** | Overwinter+: non-coinbase transactions must have `nExpiryHeight` <= 499999999, and MUST NOT be mined at a block height greater than their `nExpiryHeight` (ZIP-203) |
| **Gate** | Not upgrade-gated (applies to all Overwinter+ transactions), but only enforced during semantic verification |
| **Risk** | Low — this is a semantic-only check skipped for checkpoint-verified blocks. The correct expiry heights are implicitly guaranteed by checkpoint hash verification. |

### Note: Transaction Network Upgrade Consistency

The `check_transaction_network_upgrade_consistency` check at `zebra-consensus/src/block/check.rs:411` (called via `merkle_root_validity()`) validates that all transactions with a `nConsensusBranchId` field match the expected network upgrade for the block height. This check is called in **both** the semantic block verifier and the checkpoint verifier (`zebra-consensus/src/checkpoint.rs:626`), so it IS enforced for pre-Canopy blocks.

## Checks NOT Gated (Always Enforced When Semantic Verification Runs)

These checks apply to all blocks but are still **skipped by checkpoint verification**:

| Check | Location |
|-------|----------|
| Sprout JoinSplit proofs (Groth16) | `zebra-consensus/src/transaction.rs:1095-1159` |
| Sapling bundle proofs | `zebra-consensus/src/transaction.rs:1161-1224` |
| Orchard bundle proofs (Halo2) | `zebra-consensus/src/transaction.rs:1226-1253` |
| Transparent script verification | `zebra-consensus/src/transaction.rs:1065-1093` |
| Block sigops limit (MAX_BLOCK_SIGOPS = 20,000) | `zebra-consensus/src/block.rs:128` (enforced at `block.rs:307`) |
| Non-coinbase expiry height (Finding #12) | `zebra-consensus/src/transaction/check.rs:414-439` |

These are always enforced for blocks that go through semantic verification (post-checkpoint), but are **not** enforced for checkpoint-verified blocks.

## Summary

### Risk Assessment

**Two explicitly documented validation gaps exist**:
- **Finding #1**: Pre-Heartwood coinbase transactions with Sapling/Orchard Output descriptions are not rejected. The code explicitly notes this is intentional because these blocks are always checkpointed.
- **Finding #11**: Sapling note commitment tree root contextual equality validation (Sapling/Blossom) and chain history root validation (Heartwood/Canopy) are skipped with an explicit code comment acknowledging the checkpoint dependency. Structural parsing of the commitment bytes is still performed at the chain type level.

**All other upgrade-gated checks** fall into two categories:
1. **Rules that don't apply pre-Canopy** (#3, #5, #6, #7, #8, #9, #10) — these are correctly gated because the rules themselves were introduced at later network upgrades.
2. **Rules that apply between their activation and Canopy** (#2, #4, #11, #12) — these are covered by the mandatory checkpoint range and the correct chain is guaranteed by checkpoint hash verification.

### Key Concern for Future Changes

If the mandatory checkpoint height is ever lowered below Canopy activation, or if Zebra needs to validate historical blocks without checkpoints (e.g., for a new testnet or custom network), the following checks would need to be added or verified:

1. Pre-Heartwood coinbase Output description prohibition (Finding #1 — currently explicitly skipped)
2. Heartwood-to-Canopy coinbase output decryption (Finding #2)
3. Pre-first-halving Founders Reward validation (Finding #4 — the code exists but is not exercised during checkpoint sync; note this only applies before the first halving height, not all pre-Canopy blocks)
4. Sapling note commitment tree root contextual equality validation for Sapling/Blossom blocks (Finding #11 — structural parsing exists but contextual equality check is explicitly skipped)
5. Chain history root (ZIP-221) contextual equality validation for Heartwood-to-Canopy blocks (Finding #11)
6. History tree construction for Heartwood+ blocks
7. Non-coinbase expiry height enforcement (Finding #12 — semantic-only, not enforced during checkpoint sync)
8. Pre-Sapling transaction version handling — semantic verification rejects V1/V2/V3 at `zebra-consensus/src/transaction.rs:497`, and `zebra-chain/src/transaction.rs:72` does not validate pre-Sapling types. If historical blocks go through semantic verification, these version checks would need review.
9. All "always enforced" semantic checks (proofs, scripts, sigops) would need to work correctly for older transaction formats

### Recommendation

The current approach is sound for Mainnet and standard Testnet, where the mandatory checkpoint height is just before Canopy. The reliance on checkpoint verification for pre-Canopy blocks is well-understood and explicitly documented.

For custom testnets or any scenario where the checkpoint list may not cover pre-Canopy heights, consider adding the pre-Heartwood coinbase Output description check (Finding #1) as a defensive measure, since it's the only known gap that isn't covered by an upgrade gate.
