# SwiftSync-Style Spentness Hints in Zebra — Design (rev 5)

Status: decisions locked with the author · 2026-08-17 (rev 5)
Spec basis: [zcash/zips#1346](https://github.com/zcash/zips/pull/1346) at head `c593c65`.
Rev 2 incorporates today's spentness changes: `f3d170c` (bind hints to the trusted
commitment's reach, drop redundant checks), `5aeb43f` (one hash over the whole hint
bitmap, separate from entry chunks), and `c593c65` (snapshots keep all pool balances).

## 0. Decisions (2026-08-17 design session)

1. **Engine reuse:** the Phase-5 client is #10725's `Engine` (ring-buffer window,
   weighted parallel fetch, two-tier commit, pipelined verification), with its
   fetch layer swapped to `get-block-range` streams and its chunk source swapped
   to the draft's known-hash list.
2. **Hint path reuse:** hinted processing grows out of `ibd-utxo-write-elision`
   (intra-window elision generalized to the whole span via the bitmap) and
   #10725's survivorship-set verification (generalized to the salted 256-bit
   aggregate).
3. **Address indexes:** no address-index writes below the final checkpoint during
   hinted IBD; a background pass over stored blocks builds them afterwards using
   the existing DB-upgrade machinery.
4. **Frontiers:** propose optional note-commitment-tree frontier artifacts to the
   draft (content-addressed over `get-object`, verified against the following
   block's commitments, failure-not-fraud); reuse #10725's frontier fold/verify.
5. **Database format:** one combined **28.1.0** in stack PR 3: the sync-metadata
   column family, the `known_hash_chunk` column family from #10725, and a
   per-height cumulative transparent-output count.
6. **Distribution is P2P-only:** the known-hash list is reassembled via
   `get-hashes` against pinned per-range chunk hashes; hint and frontier
   artifacts come over `get-object` from `NODE_SYNC_ARTIFACTS` peers. No
   release-asset path. Cold start therefore depends on v2 seeders and
   `initial_v2_peers` carrying artifacts — the DNS-seeder work moves up.
7. **Sync order — headers-first, hints up front:** sync headers to the max
   checkpoint height first; fetch the hint artifact and verify its hash against
   the pinned commitment, and its structure (bit count vs the cumulative output
   counts in the entry chunks), **before any body download**. Bodies then stream
   with hints applied. The aggregate-zero check remains terminal (inherent to
   the construction), but recovery is now bodies-only: a failed aggregate
   re-downloads bodies without hints while keeping the verified header chain.
8. **Sequencing:** rework the open five-PR stack in place (PR 3 absorbs the
   combined format; the engine + hinted sync land in the reworked stack, at most
   one PR added). No follow-on stack.
9a. **Content-addressed serving; capture at frontier passage (2026-08-17,
   later session):** `get-object` requests are keyed by hash, never by
   height, so a serving node never computes "spentness as of height H" —
   it answers from a stored-artifacts lookup ({pinned hash → CF row} built
   from the compiled constants). The bitmap artifact is captured at the only
   moment it is cheap: when the node's own sync frontier passes the pinned
   checkpoint height, one ordered scan of `utxo_by_out_loc` (already in
   canonical bit order) against the cumulative output counts produces the
   bitmap (~25 MB), stored in a `spentness_hint` column family keyed by
   height. A node that hint-synced stores the artifact it used. Nodes
   already past the pinned height never reconstruct (that would need
   per-output spending heights); they fetch-verify-store from peers to
   serve onward. Known-hash chunks serve the same way: hash → chunk index
   via the pinned constants, bytes read-or-regenerated from state.
   Also walked back (same session): the `zebra-known-hashes` data crates
   and all file/crate distribution — chunks travel only over the P2P
   protocol, verified by `KnownHashListSpec::verify_chunk_bytes`; the
   current pinned constants are re-pinned to v2 chunk hashes by the release
   flow, and local multi-node tests pin from a trusted local node
   (config-supplied pins are honored only on networks with no compiled
   spec, i.e. Regtest and custom testnets).
9. **Pinned hash replaces the aggregate in production (2026-08-17, later
   session):** the release pins a `MaxCheckpoint { height, hash,
   spentness_hash }` constant per network. The node downloads the whole hint
   bitmap (over `get-object`, where `spentness_hash` is the content address),
   hashes it, and verifies it against the constant before any body download —
   the pinned bitmap hash carries exactly the trust of the pinned checkpoint
   hash, and a bitmap that fails the hash never influences state. This
   eliminates the adversarial threat the per-attempt salt defended against
   (the bitmap cannot be attacker-chosen at all), so production sync runs **no
   salted aggregate**. The aggregate survives only as an opt-in verification
   mode: the artifact-generation pipeline and a CI sync job run it so a
   generation or replay bug is a detected failure before release, not silent
   UTXO-set corruption on users' nodes. Consistency between `MaxCheckpoint`
   and the embedded checkpoint list is asserted by a unit test. This is spec
   feedback for the draft ZIP: with a pinned whole-artifact hash, the salted
   aggregate can move from mandatory to a verification profile.

## Value coverage under "spends never look up" (port-plan follow-up)

The elision port analysis flags spend-value coverage as the sharpest edge:
today the spent `Utxo`'s value feeds value-pool and address-balance math, and
whole-span hinted elision removes the read that supplied it. Resolution:

- **Address balances** are not computed below the final checkpoint at all
  (decision 3): the backfill pass reads stored blocks after sync.
- **Per-block value pools** below the final checkpoint are vouched for by the
  trusted commitment and are not re-verified per block (same regime that
  already skips script and proof checks).
- **The transparent pool balance at the checkpoint** is the sum of the
  surviving UTXO set's values, which accumulates from hinted-unspent creates
  alone (checked arithmetic): hinted-spent outputs never contribute, and no
  spend value is ever needed. Shielded pool balances accumulate from value
  balances in the block data as today.
- Above the checkpoint, pool tracking starts from these computed balances and
  runs exactly as today.

So the hinted path needs no spend-value source at all; `lookup_spent_utxos`
and its fallbacks are simply not called below the final checkpoint.

## 1. The spec model, as of head

Spentness hints let a node process blocks **below the final checkpoint** without a
single random read against the UTXO set or nullifier sets, replacing per-item
lookups with one salted, order-independent accumulator check per set.

**Scope.** "Hints apply only within the reach of the trusted commitment: a node
MUST NOT apply them above the final checkpoint." Everything above the final
checkpoint validates exactly as today. This is the decisive simplification of
today's revisions: below the final checkpoint, the commitment already vouches for
the chain, so per-spend consensus rules (maturity, shielded-coinbase) need no
independent re-verification — which is why the earlier per-output
creation-height/coinbase metadata extension was **removed** from the artifact.

**Commitment.** One SHA-256 hash over the whole hint artifact: "the hint bits for
every transparent output created at or below the final checkpoint … serialized in
canonical order", fetched and "verified whole against this hash". This hash lives
alongside — but separate from — the known-hash list's per-range entry-chunk hashes:
entry-chunk hashes stay stable as coverage extends; the hint hash rotates with
every final-checkpoint bump (the bits are relative to the terminal height).

**Accumulator.** Per synchronization attempt: a fresh 32-byte cryptographically
random salt; one 256-bit wrapping-add aggregate per set, summing keyed
BLAKE2b-256 (salt as key, distinct personalization per aggregate) of each item.
Machine-word accumulators are prohibited. An attacker who cannot predict the salt
cannot craft items whose hashes cancel.

**Transparent outputs.** One hint bit per output created in the span, canonical
order (height, tx index, output index):
- hinted **unspent** → written to the UTXO set on creation; never looked up or
  deleted during the span;
- hinted **spent** → never stored; its **outpoint** is added to the aggregate;
- every transparent input subtracts its referenced outpoint — no lookup.
At the terminal height the aggregate MUST be exactly zero: the constructed UTXO
set is then exactly the never-spent outputs, "without a single random read".

**Nullifiers.** No per-item hint (the sets never shrink). Snapshot backfill uses
the snapshot's own sets as the hint: every member added to the pool's aggregate,
every nullifier revealed by the span's blocks subtracted; zero at the terminal
height verifies the snapshot's sets against the chain. The former
strictly-increasing distinctness check on the streamed snapshot sets is
**dropped**: double-reveal below the final checkpoint is excluded by the trusted
commitment, and multiset equality transfers the revealed nullifiers' distinctness
to the snapshot set.

**Pool balances.** Snapshots keep **all** chain value pool balances (including
transparent), "the starting point for the pool-balance consensus rules above `H`";
the node verifies them by "recomputing them over the replayed span (with checked
arithmetic) and comparing them to the snapshot's".

**Failure.** A nonzero aggregate means hints, snapshot sets, or delivered blocks
were wrong: discard the affected state and fall back to checkpointed
synchronization without hints. "Wrong hints produce failure, not fraud."

**Distribution.** No new wire messages: the hint bitmap is a content-addressed
artifact over `get-object` (or bundles/mirrors), accepted only on hash match.

## 2. Why this fits Zebra unusually well

- Zebra's IBD is CPU-bound on the state write path, dominated by per-output UTXO
  and balance reads at commit time (ibd-engine profiling). Hinted processing
  deletes exactly those reads for the whole below-checkpoint span — which is ~all
  of IBD.
- Below-final-checkpoint is already Zebra's reduced-verification regime (the
  checkpoint verifier skips scripts and proofs there). The spec now scopes hints
  to precisely that regime, so **genesis-to-checkpoint sync needs only the
  transparent bitmap**: with no snapshot to discharge and double-reveal excluded
  by the commitment, nullifiers are plain writes with no aggregate at all. (This
  resolves rev 1's open question 2 — genesis mode falls out for free.)
- The aggregate is a mergeable value: wrapping addition commutes, so each parallel
  pipeline lane owns a local aggregate and the span coordinator sums them at the
  join. Single-owner by construction — no shared state, which also satisfies the
  no-locks concurrency policy without any design contortion.

## 3. The accumulator primitive (`zebra-chain`)

```rust
/// A salted, order-independent multiset accumulator (draft sync ZIP, Spentness Hints).
pub struct SpentnessAggregate {
    /// Keyed-hash template: BLAKE2b-256, the attempt's salt as key,
    /// a fixed per-aggregate personalization.
    params: blake2b_simd::Params,
    /// Additive aggregate: little-endian limbs, arithmetic mod 2^256.
    limbs: [u64; 4],
}

impl SpentnessAggregate {
    pub fn new(salt: &SyncSalt, personalization: Personalization) -> Self;
    pub fn add(&mut self, item: &[u8]);
    pub fn subtract(&mut self, item: &[u8]);      // add the 2^256 complement
    pub fn is_zero(&self) -> bool;
    /// Wrapping addition commutes: lanes keep local aggregates, merged at the join.
    pub fn merge(&mut self, other: &Self);
}
```

- `SyncSalt`: 32 bytes from `OsRng`, one per synchronization attempt; never
  persisted, logged, or reused across attempts.
- `Personalization`: an enum for the aggregates the draft defines (transparent
  outpoints; one per nullifier pool for snapshot backfill), mapped to the draft's
  exact 16-byte strings — the draft fixed a personalization example in `54b5324`,
  so pin whatever it specifies and add a conformance test against the spec vector.
- Item encodings: transparent = the 36-byte outpoint, exactly as serialized on the
  wire; nullifier = the 32-byte nullifier. No metadata — rev 1's `SpentMeta` is
  gone with the spec change.
- Implementation: `[u64; 4]` add/sub with manual carries; `blake2b_simd` is
  already a workspace dependency. Property tests: commutativity, merge ≡
  interleaved adds, add/subtract cancellation iff equal multisets, no cancellation
  under item tweaks.

## 4. The hint artifact

**Format.** The draft's canonical serialization: the bitmap of hint bits for every
transparent output created at or below the final checkpoint, canonical order
(height, tx index, output index), fetched and verified **whole** against the
single committed SHA-256. Rough size: one bit per transparent output ever created
— order tens of megabytes at current mainnet history — so a single resumable
`get-object` fetch (byte ranges are already served) or a bundled release file.

**Pinning.** The known-hashes crates pin, per release:
1. the per-range entry-chunk hashes of the known-hash list (stable: existing
   ranges never change, new releases append ranges), and
2. the one spentness-hint hash for the release's final checkpoint (rotates every
   release that moves the checkpoint).

**Consumption.** The whole bitmap verifies before use (hash of the artifact
file), then maps read-only into the pipeline as an indexed bit lookup: the n-th
created transparent output's bit, with a per-height starting index derived while
walking the span (no random artifact access needed — creation order is exactly
walk order).

**Generation.** A `zebra-utils` subcommand (`zebra-hint-gen`) walks a synced
state and emits the bitmap + its hash; release CI cross-checks two independently
generated artifacts byte-for-byte, and checks the hash against the pinned value.

## 5. Pipeline and state integration

Hinted mode is a property of the below-final-checkpoint span in the
checkpointed/ibd-engine pipeline, enabled when the verified bitmap is available;
absent or failed hints degrade to today's checkpointed sync.

Per block, per lane:
- transparent outputs: bitmap says unspent → blind UTXO write (append-only, no
  read); spent → skip the write, `aggregate.add(outpoint)`;
- transparent inputs: `aggregate.subtract(outpoint)`; no existence, maturity, or
  coinbase lookup below the final checkpoint (covered by the commitment);
- nullifiers: plain writes to the nullifier column families; no duplicate lookup.
  Snapshot-anchored spans additionally subtract each revealed nullifier from the
  pool aggregate (the added side is the snapshot set, streamed once);
- pool balances: recomputed per block with checked arithmetic in height order (the
  commit stage is sequential even when verification is parallel); for
  snapshot-anchored spans, compared at `H` against the snapshot's balances — all
  pools, including transparent.

Terminal verification, genesis-anchored (ordinary IBD): the transparent aggregate
is zero → the constructed UTXO set is correct; done. Snapshot-anchored backfill:
transparent and per-pool nullifier aggregates all zero, plus the security-review
reconciliation — whole UTXO entries (amounts included), Sprout tree, and the
snapshot's pool balances against the recomputed ones.

**Verification order and recovery (decision 7).** Headers sync to the max
checkpoint height first; the hint artifact's hash and structure verify before
any body download, so artifact-source failures cost nothing. The
`hinted_span_unverified` marker is set before the first hinted body write and
cleared when the terminal aggregates and reconciliation pass. On an aggregate
failure, the header chain and known-hash state are kept, hinted body-derived
state is discarded, and body sync restarts without hints. Failure is a liveness
cost only, per the draft.

**Concurrency.** Aggregates and the bitmap cursor are owned values in pipeline
lanes and the span coordinator; the only cross-lane operation is `merge` at the
join. No shared cells, no locks.

## 6. Milestones

1. **M1 — primitive:** `SpentnessAggregate` + `SyncSalt` + personalization
   conformance vectors in `zebra-chain`.
2. **M2 — artifact:** bitmap format + indexed reader + `zebra-hint-gen` +
   cross-generation CI check.
3. **M3 — hinted genesis IBD:** blind-write transparent path + aggregate plumbing
   + unverified-span marker in zebra-state and the sync pipeline. This is the
   headline IBD win and needs no snapshot machinery.
4. **M4 — snapshot backfill:** nullifier-set streaming into aggregates, full
   reconciliation (UTXO entries, Sprout tree, pool balances), snapshot manifest
   binding.
5. **M5 — distribution:** known-hashes crate pinning (entry-chunk hashes + hint
   hash), `get-object` fetch/resume, end-to-end Regtest two-node test with
   generated artifacts.

M1–M2 are independent of the v2 PR stack; M3 converges with ibd-engine; M4–M5
depend on the snapshot manifest sections of the draft stabilizing.

## 7. Open questions / spec feedback

1. **Bitmap indexing aid:** the artifact is one undifferentiated bitmap; an
   optional per-height (or per-range) starting-index side table would let a node
   resume a partially processed span without rewalking. Local computation is
   cheap, so this may not merit spec text — implementation detail for us.
2. **Personalization strings:** confirm the draft pins exact 16-byte values for
   each aggregate (the example was fixed in `54b5324`; normative values should be
   table-listed).
3. **Hint hash location:** confirm the hint hash is compiled alongside the
   known-hash list pins (per `5aeb43f` it is committed "separately from the entry
   chunks" — separate hash, same pinning mechanism?), so release tooling knows
   where it lives.
4. **Artifact staleness window:** a node syncing with an older release's hint
   hash covers a lower final checkpoint; text confirming that hints for a lower
   checkpoint remain valid (just shorter) with normal validation above would
   remove any ambiguity.
