# NU6.3 / Ironwood — Implementation Plan for Zebra

Status: **Phases 1, 2, 3 & 4-foundation landed** · Branch: `nu63-ironwood` (worktree, off `main` @ v5.1.0)
Sources: zcash/zips#1300 (spec refactor), zcash/zips#1301 (v6 tx format),
zcash/ironwood (the Ironwood Book).

## Decisions (locked)
- **Ironwood activates at NU6.3** (a new `NetworkUpgrade::Nu6_3` between `Nu6_2`
  and `Nu7`), *not* at NU7.
- **v6 transaction drops `zip233_amount`** — v6 is "v5 + an Ironwood bundle",
  with no ZIP-233 burn field. The existing `Transaction::V6`
  (`cfg(zcash_unstable="nu7", feature="tx_v6")`) must be reshaped accordingly in
  Phase 2.

## Status log
- **Phase 1 ✅** — `Nu6_3` network-upgrade scaffolding landed: enum variant
  (+`serde "NU6.3"`), test-gated placeholder branch id `0xfffffffe`, testnet
  `ConfiguredActivationHeights.nu6_3`, all exhaustive matches across
  zebra-chain/-consensus/-network/-rpc, and the unscheduled-upgrade tests
  (now `iter - 2`). No mainnet/testnet activation height yet (mirrors `Nu7`).
  `cargo check --workspace --all-targets`, parameter/network/halo2 tests, fmt,
  and clippy `-D warnings` all green.
- **Phase 3 ✅** — sixth chain value pool `ironwood` added to `ValueBalance<C>`:
  field, `from_ironwood_amount`/`ironwood_amount`/`set_ironwood_value_balance`,
  `zero`/`constrain`/`Add`/`Sub`/`Neg`/arbitrary, and inclusion in
  `remaining_transaction_value`. Serialization appends ironwood at bytes
  `40..48`; `to_bytes` is now `[u8; 48]` and `from_bytes` accepts `32|40|48`
  (old DBs read with ironwood = 0). On-disk consumers updated:
  `IntoDisk for ValueBalance` (`[u8; 48]`) and `BlockInfo` (44-byte old / 52-byte
  new layouts). **DB format bumped to 27.1.0** (minor; compatible read code).
  Value-pool raw-data snapshots regenerated (verified: pure ironwood-zero
  widening, all real values preserved). Workspace all-targets, fmt, clippy
  `-D warnings`, and value_balance/disk_format tests green.
  Note: getblockchaininfo's `valuePools` is still a 5-pool array (zcashd-compat)
  — adding the ironwood pool there is deferred to Phase 9.
- **Phase 4 (foundation) ✅** — `Transaction` ironwood accessor surface added,
  mirroring the orchard accessors: `ironwood_shielded_data` (returns `None`
  until the v6 bundle field lands in Phase 2), `ironwood_actions`,
  `ironwood_nullifiers`, `ironwood_note_commitments`, `ironwood_flags`,
  `has_ironwood_shielded_data`, and `ironwood_value_balance` — the last now
  folded into `value_balance_from_outputs` (zero today). Ironwood reuses the
  Orchard tree/nullifier *types* (same Pallas/Sinsemilla MerkleCRH), so no
  duplicated tree module is needed; the pools differ only by storage instance.

- **Phase 2 ✅ (gated)** — `Transaction::V6` reshaped to the Ironwood format under
  its `zcash_unstable="nu7"` + `tx_v6` gate: dropped `zip233_amount`, added
  `ironwood_shielded_data: Option<orchard::ShieldedData>` serialized after the
  orchard bundle (identical wire shape). `ironwood_shielded_data()` now returns
  the v6 field; `has_shielded_inputs`/`has_shielded_outputs` count ironwood
  actions (so an ironwood-only v6 tx has inputs/outputs). Removed the
  `zip233_amount()`/`has_zip233_amount()` accessors and the consensus burn
  subtraction in the tx-fee path; simplified `has_inputs_and_outputs`. Neutralized
  the orphaned `zip235` NSM burn enforcement in `block/check.rs` (burn treated as
  zero, with a TODO to re-source it post-Ironwood) and removed the three obsolete
  `zip233` NSM tests.
  - Validated: default `--workspace --all-targets` build + clippy `-D warnings` +
    zebra-chain/-consensus tests green; `nu7`+`tx_v6` compiles for
    zebra-chain/-state/-consensus (lib + tests).
  - **Known gaps (upstream librustzcash):** `to_librustzcash` is a
    serialize→parse roundtrip, so v6 `txid`/`auth_digest`/`sighash` fail at
    *runtime* until `zcash_primitives`' v6 parser matches Ironwood (v6 wire
    (de)serialization in Zebra is self-contained and correct). The `zebra-rpc`
    `tx_v6` block-template/mining path is **pre-broken on `main`** (verified by
    stashing) and relies on librustzcash's `set_zip233_amount`; left as-is.

### Remaining (Phase 4 state storage, now unblocked by Phase 2)
With v6 carrying an Ironwood bundle, the note-commitment-tree / nullifier-set
state storage (non-finalized `Chain` maps + finalized column families, reusing
orchard tree/nullifier types) can now consume `ironwood_note_commitments()` /
`ironwood_nullifiers()`. This is the next concrete chunk.

---

## 1. What NU6.3 / Ironwood is

Ironwood is a **new shielded value pool** that *reuses the Orchard action +
Halo2 proof system unchanged* but maintains its **own note commitment tree,
nullifier set, and chain value-pool balance**. It is delivered by a new
**version 6 transaction format** = "v5 + an Ironwood bundle". Motivation:
restore confidence in supply integrity; after activation, **no new value can
enter Orchard** — all new shielded value routes through Ironwood (legacy Orchard
notes stay spendable; Orchard balance may only decrease).

Concrete deltas:

- **v6 transaction**: transparent → sapling → orchard → **ironwood** bundle.
  The Ironwood bundle is byte-identical in shape to the Orchard bundle
  (820-byte actions, flags, valueBalance, anchor, proofs, spend-auth sigs,
  binding sig). Components are omitted entirely when their action count is 0.
- **New flag bit 2 `enableCrossAddress`** on both Orchard and Ironwood flags
  (previously reserved-0). Affirmative sense (1 = enabled). Coinbase must keep
  it 0; Orchard component must be empty in coinbase; Ironwood spends disabled in
  coinbase.
- **New note plaintext format** (ZIP 2005, lead byte `0x03`, quantum-recoverable)
  — Ironwood outputs only; Orchard never uses it. *Consensus-irrelevant to a
  validator node* (plaintext is in the encrypted ciphertext); matters to wallets.
- **txid / auth digest changes** (ZIP 244 extension): anchors move from
  *effecting* data to *authorizing* data for Sapling/Orchard/Ironwood (enables
  re-anchoring without changing txid). New 16-byte personalizations for the
  ironwood sub-digests and the modified sapling/orchard ones (`*_v6`).
- **ZIP 209**: a 6th chain value pool ("ironwood"); must stay ≥ 0; Orchard must
  also stay ≥ 0 post-activation.
- **ZIP 221**: history-tree (MMR) node gains 3 fields — `hashEarliestIronwoodRoot`,
  `hashLatestIronwoodRoot`, `nIronwoodTxCount` (the V3 node format).

---

## 2. Blockers & open questions (resolve before / during coding)

These are the reason most of this plan **cannot be finished today**:

1. **NU6.3 vs NU7 naming is unresolved upstream.** The ZIP PRs say *NU6.3*; the
   Ironwood Book says *NU7*. We must pin which network-upgrade variant Ironwood
   activates at before wiring it. The codebase already has `Nu7` (test branch id
   `0xffffffff`) and an `Nu6_2` as the latest *real* upgrade.
2. **All consensus params are placeholders**: consensus branch ID, v6
   version-group ID (Zebra currently has `TX_V6_VERSION_GROUP_ID = 0xFFFF_FFFF`
   placeholder), mainnet/testnet activation heights, protocol (peer) version.
3. **The existing `V6` variant conflicts with Ironwood's v6.** Zebra's current
   `Transaction::V6` (`cfg(all(zcash_unstable="nu7", feature="tx_v6"))`) carries
   `zip233_amount` (the older ZIP-233 burn NU7 draft) and has **no Ironwood
   bundle**. Ironwood's v6 *excludes* ZIP-233 and *adds* an Ironwood bundle. One
   of these wins, or v6 carries both. This must be reconciled against whatever
   `zcash_primitives` finalizes.
4. **Upstream librustzcash dependency (the big one).** Zebra computes txids,
   sighashes, auth digests, and verifies Orchard/Halo2 proofs by converting to
   `zcash_primitives::Transaction` (`Transaction::to_librustzcash`) and calling
   into the `orchard` crate. Ironwood support therefore requires, *upstream
   first*:
   - `zcash_protocol`: `NetworkUpgrade::Nu6_3` (or Nu7) + branch id. Our
     `From<zcash_protocol::…::NetworkUpgrade>` impl won't compile without it.
   - `zcash_primitives`: v6 `Transaction` with an Ironwood bundle, plus the new
     txid/auth-digest personalizations and the anchor-to-authorizing move.
   - `orchard` crate: Ironwood pool note machinery, `0x03` plaintext, and the
     cross-address circuit change (the `enableCrossAddress` in-circuit
     constraint).
   Until these land, Zebra can serialize/deserialize v6 and track the pool, but
   **cannot validate v6 txid/auth/proofs**.

> Action: open/track an upstream-coordination issue with the Zebra team that
> records the resolved answers to (1)–(3) and the librustzcash versions for (4).
> (You are a maintainer, so the CLAUDE.md contribution gate is bypassed — but
> this is a large, cross-team upgrade and still wants a tracking issue.)

---

## 3. Architecture note: what Zebra owns vs. delegates

| Concern | Owner |
|---|---|
| NU enum, branch ids, activation heights, protocol version | **Zebra** |
| v6 wire (de)serialization | **Zebra** |
| 6th value pool accounting + DB serialization | **Zebra** |
| Ironwood note-commitment tree + nullifier set + anchors | **Zebra** (mirrors Orchard) |
| History tree V3 node | **Zebra** (mirrors V2; may need `zcash_history::V3`) |
| Consensus gating (tx version ↔ NU, coinbase, flags, pool ≥ 0) | **Zebra** |
| txid / sighash / auth digest | **librustzcash** (via `to_librustzcash`) |
| Orchard/Ironwood proof (Halo2) verification | **librustzcash `orchard` + `halo2`** |
| Note plaintext `0x03` | **wallets** (not a validator concern) |

The practical consequence: the **mechanical "add a 6th pool / add a variant"
plumbing is large but tractable today**; the **cryptographic validation is
gated on upstream** and should be stubbed/feature-gated until the crates exist.

---

## 4. Phased implementation

Each phase is a self-contained, individually-committable unit (per repo
convention: atomic logical commits; `foo.rs` + `foo/` module layout; CHANGELOG
entry per user-visible change).

### Phase 0 — Upstream coordination (blocking, no Zebra code)
- Pin NU naming (NU6.3 vs NU7) and obtain branch id / version-group id /
  activation heights / protocol version.
- Track librustzcash versions exposing `Nu6_3`, v6 Ironwood `Transaction`, and
  the `orchard` Ironwood pool. Bump `Cargo.toml` deps when available.

### Phase 1 — Network-upgrade scaffolding  (`zebra-chain`)
Mirror exactly how `Nu6_2` was added. Files & sites:
- `parameters/network_upgrade.rs`: add `Nu6_3` enum variant (+`#[serde(rename="NU6.3")]`);
  add to `MAINNET_ACTIVATION_HEIGHTS`/`TESTNET_ACTIVATION_HEIGHTS`;
  add `CONSENSUS_BRANCH_IDS` row; add to the `target_spacing` match (~`:404`);
  add the `From<zcash_protocol::…::NetworkUpgrade>` arm (~`:528`).
- `parameters/constants.rs`: `NU6_3` height consts for mainnet (~`:82`) & testnet (~`:56`).
- `parameters/network/testnet.rs`: `ConfiguredActivationHeights.nu6_3` field
  (+rename), the `From<&BTreeMap>` arm (~`:192`), `for_regtest` destructure/return,
  and the `activation_heights` chain (~`:635`).
- `parameters/tests.rs`: add to `NETWORK_UPGRADES_IN_ORDER` (~`:273`).
- Compile-forced exhaustive matches to extend: `block/commitment.rs:142`,
  `history_tree.rs:108,178`, `primitives/zcash_history.rs:281`,
  `transaction/arbitrary.rs:802`.
- `zebra-network/src/protocol/external/types.rs`: protocol-version mapping
  (`:119`), coverage test (`:244`), test array (`:259`); `constants.rs` "current
  upgrade" TODO (~`:426`).
- `zebra-consensus`: `primitives/halo2.rs:266` (route `Nu6_3` to the
  post-NU6.2 verifier, same as Nu7) + its test loop.

### Phase 2 — v6 transaction format & ZIP-233 reconciliation  (`zebra-chain`)
- **Decide v6 shape** (Blocker #3). Most likely outcome: v6 = v5 +
  `ironwood_shielded_data: Option<orchard::ShieldedData>` and **drop**
  `zip233_amount` (or keep behind a separate cfg if NU7/ZIP-233 is still live).
  Keep the Ironwood bundle as a *reuse* of `orchard::ShieldedData` (identical
  wire shape) — ideally a thin newtype/alias so trees & nullifiers stay
  type-distinct (`IronwoodShieldedData`), avoiding accidental cross-pool mixing.
- `transaction.rs`: update the `V6` variant fields; extend **every** exhaustive
  `match` (the research enumerated ~40 accessors + ~9 `*_mut` test helpers).
  Add `ironwood_shielded_data()`, `ironwood_value_balance()`,
  `ironwood_nullifiers()`, `ironwood_note_commitments()`, `ironwood_anchors()`,
  and fold ironwood into `value_balance_from_outputs()`.
- `transaction/serialize.rs`: extend `V6` (de)serialize to write/read the
  Ironwood bundle *after* Orchard, reusing the existing Orchard
  `Option<ShieldedData>` codec; set the real `TX_V6_VERSION_GROUP_ID`
  (`parameters/transaction.rs`); validate v6 ⇒ NU6.3+.
- `transaction/arbitrary.rs`: v6 strategy generates Ironwood bundles.

### Phase 3 — Chain value pool: the 6th pool  (`zebra-chain`, **DB format bump**)
`value_balance.rs`:
- add `ironwood: Amount<C>` to `ValueBalance<C>`; `from_ironwood_amount`,
  `ironwood_amount`, `set_ironwood_value_balance`.
- include ironwood in `remaining_transaction_value`, `Add/Sub/AddAssign/SubAssign`.
- **`to_bytes`/`from_bytes`: 40 → 48 bytes** (6 pools). This is a state DB format
  change → bump `DATABASE_FORMAT_VERSION` and add a migration (Phase 8).
- `ValueBalanceError::Ironwood(amount::Error)`.

### Phase 4 — Ironwood note-commitment tree & nullifiers  (`zebra-chain` + `zebra-state`)
- `zebra-chain`: an `ironwood` module mirroring `orchard::tree` (incremental
  Merkle tree, `Root`, `Node`, subtree support). If the tree is parameter-
  identical to Orchard, alias/reuse; keep the *type* distinct.
- `zebra-state` non-finalized `chain.rs`: `ironwood_trees_by_height`, anchors,
  subtrees; `ironwood_tree()`, `add_ironwood_tree_and_anchor()`, remove paths;
  nullifier set insertion + double-spend checks (mirror Orchard).
- `zebra-state` finalized (rocksdb) column families for ironwood tree/anchors/
  nullifiers/subtrees + reads in `service/read/tree.rs`
  (`ironwood_tree`, `ironwood_subtrees`).

### Phase 5 — History tree V3 (ZIP 221)  (`zebra-chain`)
- `primitives/zcash_history.rs`: if `zcash_history` exposes `V3`, add
  `impl Version for V3` adding `start/end_ironwood_root` + `ironwood_tx` (mirror
  the V2/Orchard impl ~`:304`). Otherwise carry a Zebra-local V3 node.
- `history_tree.rs`: `InnerHistoryTree` gains an `IronwoodOnward` arm; route
  `Nu6_3+` to it in `from_cache`/`from_block`.
- `block/commitment.rs`: ensure `hashBlockCommitments`/`ChainHistoryRoot` paths
  cover `Nu6_3`.

### Phase 6 — Consensus verification  (`zebra-consensus`)
- `transaction.rs`: tx-version ↔ NU gating —
  `verify_v5_transaction_network_upgrade` add `Nu6_3`;
  add `verify_v6_transaction` that (a) validates Orchard bundle, (b) validates
  the **Ironwood bundle** (new `verify_ironwood_bundle()`, mirroring
  `verify_orchard_bundle`, routed through the Halo2 verifier once `orchard`
  exposes the Ironwood circuit), (c) enforces coinbase rules (empty Orchard,
  `enableSpendsIronwood=0`, `enableCrossAddress=0`), (d) enforces flag bits 3–7
  = 0.
- `check.rs`: `consensus_branch_id` already generic; confirm v6 path.
- Block-level: enforce **Orchard & Ironwood chain value pool ≥ 0** post-NU6.3
  (extend the existing pool-balance check that today covers sprout/sapling/
  orchard/deferred).
- `primitives/halo2.rs`: Ironwood proof verifier wiring (Phase 1 stub → real).
- `transaction/tests/prop.rs:365`: version map includes `Nu6_3`.

### Phase 7 — txid / sighash / auth digest  (`zebra-chain`, mostly via librustzcash)
- Once `zcash_primitives` ships v6: `txid.rs` `txid_v6` and `auth_digest.rs`
  should mostly "just work" through `to_librustzcash`. Verify the new `*_v6`
  personalizations and the anchor→authorizing move are reflected.
- `primitives/zcash_primitives.rs`: extend `to_librustzcash` and
  `PrecomputedTxData` to populate the Ironwood bundle; add an `ironwood_bundle()`
  accessor for the verifier.

### Phase 8 — State DB migration + format-version bump  (`zebra-state`)
- Bump `DATABASE_FORMAT_VERSION` (`zebra-state/src/constants.rs`).
- Migrations: widen value-pool serialization (40→48B), create Ironwood CFs,
  initialise Ironwood pool/tree to empty at the activation boundary.

### Phase 9 — RPC  (`zebra-rpc`)
- `methods/trees.rs`: `GetTreestateResponse.ironwood: Treestate` (z_gettreestate).
- `methods/types/transaction.rs`: `TransactionObject` ironwood actions field
  (parallel to `orchard`), for getrawtransaction/getblock verbose.
- `get_block_template/proposal.rs:218`: cover `Nu6_3` in commitment match.

### Phase 10 — Tests & validation
- Unit/property: value_balance round-trip (48B), v6 serialize round-trip,
  NU-ordering tests, tree/nullifier double-spend.
- Conformance: ZIP-244 v6 txid/auth test vectors once published; replay against
  librustzcash test vectors.
- Integration: testnet (then mainnet) full-sync across the activation height once
  params exist; regtest config with `nu6_3` activation for local e2e.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`, then nextest sync profiles.

---

## 5. Sequencing & risk

```
Phase 0 (upstream)  ──────────────►  unblocks 6,7
   │
Phase 1 (NU enum)  ─► 2 (v6 wire) ─► 3 (value pool) ─► 4 (trees/nullifiers)
                                   └► 5 (history V3)
                                                       ─► 6 (consensus) ─► 9 (RPC)
3,4 ─► 8 (DB migration + format bump)                  └► 7 (digests)
all ─► 10 (tests)
```

- **Can start now, fully (no upstream needed):** Phase 1 (using the existing
  `Nu7` placeholder-id pattern), Phase 3, Phase 4 plumbing, Phase 9 shape.
- **Blocked on librustzcash:** Phase 6 proof verification, Phase 7 digests, and
  the *final* v6 field shape in Phase 2 (gate behind `cfg`/feature until then).
- **Highest-risk areas:** value-pool DB format migration (Phase 3/8) and the
  v6/ZIP-233 reconciliation (Phase 2) — both hard to reverse once shipped.

## 6. Recommended first concrete step
Land **Phase 1** behind the existing `zcash_unstable`/feature gating (compiles,
adds the variant + all match arms, no activation height on mainnet/testnet yet),
plus a **tracking issue** capturing the Blocker resolutions. That gives a green
build to iterate Phases 2–10 against without committing to unfinalized params.
